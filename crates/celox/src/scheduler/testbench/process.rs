//! The blocks of one testbench process: its release from a reset, its
//! turn, and what each request of its kernel does.
use super::*;

/// The blocks of process `i`: release the reset it asserted, pick it when
/// it is ready, run its kernel and serve the request the kernel returns
/// with, and save its locals.
pub(super) fn generate_process(
    g: &mut Generator<'_>,
    cx: &Bench<'_>,
    cones: &mut Cones<'_>,
    i: usize,
    kernel: &TestbenchKernel<AbsoluteAddr>,
) {
    let &Bench {
        program,
        tb,
        m,
        edge,
        end,
        overflow,
        ref release_check,
        ref run,
        ref pick,
        ref released,
        ref save,
        ..
    } = cx;
    let p = m.processes[i];
    let next = pick.get(i + 1).copied().unwrap_or(edge);
    g.b.switch_to_block(release_check[i]);
    let state = g.load(p[0], 64);
    let releasing = g.equals(state, 3);
    let wake = g.load(p[3], 64);
    let now = g.load(m.control[TB_NOW], 64);
    let due = g.binary(1, wake, BinaryOp::Eq, now);
    let release = g.binary(1, due, BinaryOp::And, releasing);
    let wake_block = g.b.new_block();
    branch(g, release, wake_block, released[i]);
    g.b.switch_to_block(wake_block);
    put(g, p[0], 0);
    let mut statements = vec![];
    sites(tb.processes().nth(i).unwrap(), &mut statements);
    // Release the reset a `rst.assert` asserted: its request, plus one.
    let releases: Vec<_> = kernel
        .requests
        .iter()
        .enumerate()
        .filter_map(|(index, request)| match request {
            KernelRequest::Statement(site) => match statements[*site] {
                SemanticStatement::ResetAssert {
                    reset_signal,
                    deassert_value,
                    ..
                } => Some((index, reset_signal, *deassert_value, g.b.new_block())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    let reset = g.load(p[4], 64);
    let mut cases = vec![SIRSwitchCase {
        value: 0u64.into(),
        target: released[i],
    }];
    cases.extend(releases.iter().map(|&(index, _, _, target)| SIRSwitchCase {
        value: (index as u64 + 1).into(),
        target,
    }));
    let reset_host = g.b.new_block();
    dispatch(g, reset, cases, reset_host);
    for (_, signal, value, target) in releases {
        g.b.switch_to_block(target);
        write_reset(g, signal, value);
        put(g, p[4], 0);
        jump(g, released[i]);
    }
    g.b.switch_to_block(reset_host);
    put(g, m.control[TB_CURRENT], i as u64);
    put(g, m.control[TB_PHASE], 3 + 2 * i as u64);
    put(g, m.control[TB_EXIT], TB_RESET);
    g.b.seal_block(SIRTerminator::Return);
    g.b.switch_to_block(released[i]);
    jump(
        g,
        release_check.get(i + 1).copied().unwrap_or_else(|| pick[0]),
    );
    g.b.switch_to_block(pick[i]);
    let ready = equals_slot(g, p[0], 0);
    let ticks = g.load(m.control[TB_TICKS], 64);
    let limit = g.load(m.control[TB_LIMIT], 64);
    let active = g.binary(1, ticks, BinaryOp::LtU, limit);
    let running = equals_slot(g, m.control[TB_STOPPED], 0);
    let active = g.binary(1, active, BinaryOp::And, running);
    let ready = g.binary(1, ready, BinaryOp::And, active);
    let restore = g.b.new_block();
    branch(g, ready, restore, next);
    g.b.switch_to_block(restore);
    for &(source, saved) in &m.locals[i] {
        copy(g, program, saved, source);
    }
    jump(g, run[i]);
    g.b.switch_to_block(run[i]);
    put(g, m.control[TB_CURRENT], i as u64);
    g.b.inline_execution_unit(&program.sir.processes[kernel.entry]);
    let status = g.load(kernel.status, celox_design::PROCESS_STATUS_WIDTH);
    let is_done = g.equals(status, ProcessStatus::Done.code() as u64);
    let completed = g.b.new_block();
    let request = g.b.new_block();
    branch(g, is_done, completed, request);
    g.b.switch_to_block(completed);
    put(g, p[0], 2);
    // A lone process on one clock ends the run when it completes, as the
    // synchronous runner does, without dispatching another edge.
    if m.processes.len() == 1 && m.clocks.len() <= 1 {
        jump(g, end);
    } else {
        jump(g, save[i]);
    }
    g.b.switch_to_block(request);
    let request_value = g.load(kernel.request, 64);
    let requests: Vec<_> = kernel.requests.iter().map(|_| g.b.new_block()).collect();
    let cases = requests
        .iter()
        .enumerate()
        .map(|(index, &target)| SIRSwitchCase {
            value: celox_design::HostRequestKind::Testbench
                .encode(index as u32)
                .into(),
            target,
        })
        .collect();
    dispatch(g, request_value, cases, overflow);
    for (request_index, (request, block)) in kernel.requests.iter().zip(requests).enumerate() {
        g.b.switch_to_block(block);
        generate_request(
            g,
            cx,
            cones,
            i,
            kernel,
            &statements,
            p,
            request_index,
            request,
        );
    }
    g.b.switch_to_block(save[i]);
    for &(source, saved) in &m.locals[i] {
        copy(g, program, source, saved);
    }
    jump(g, next);
}

/// What request `request_index` of the kernel of process `i` does, in the
/// block the kernel's request dispatches to.
fn generate_request(
    g: &mut Generator<'_>,
    cx: &Bench<'_>,
    cones: &mut Cones<'_>,
    i: usize,
    kernel: &TestbenchKernel<AbsoluteAddr>,
    statements: &[&SemanticStatement<AbsoluteAddr>],
    p: [AbsoluteAddr; 5],
    request_index: usize,
    request: &KernelRequest<AbsoluteAddr>,
) {
    let &Bench {
        program,
        m,
        slots,
        deferred,
        capacity,
        finish,
        finished,
        ref run,
        ref save,
        ..
    } = cx;
    match request {
        KernelRequest::Settle { reads } => {
            let native = equals_slot(g, m.control[TB_NATIVE_SETTLE], 1);
            let settle = g.b.new_block();
            let host = g.b.new_block();
            branch(g, native, settle, host);
            g.b.switch_to_block(host);
            put(g, m.control[TB_PHASE], 2 + 2 * i as u64);
            put(g, m.control[TB_EXIT], TB_HOST);
            g.b.seal_block(SIRTerminator::Return);
            g.b.switch_to_block(settle);
            match deferred.then(|| cones.cone(reads)).flatten() {
                // With settles deferred, computing what the TB reads
                // leaves the settle pending.
                Some(units) => {
                    let dirty = g.load(slots.dirty, 8);
                    let clean = g.equals(dirty, 0);
                    let compute = g.b.new_block();
                    branch(g, clean, run[i], compute);
                    g.b.switch_to_block(compute);
                    for unit in &units {
                        g.inline(unit, finish);
                    }
                    jump(g, run[i]);
                }
                None => settle_then(g, m, run[i]),
            }
        }
        KernelRequest::Written { .. } => {
            let dirty = g.constant(1, 8);
            g.store(slots.dirty, 8, dirty);
            jump(g, run[i]);
        }
        KernelRequest::Statement(site)
            if matches!(statements[*site], SemanticStatement::ClockNext { .. }) =>
        {
            let SemanticStatement::ClockNext {
                clock_event, count, ..
            } = statements[*site]
            else {
                unreachable!()
            };
            let clock = m.events.iter().position(|a| a == clock_event).unwrap();
            let count = match count {
                ClockCount::Static(n) => g.constant(*n, 64),
                ClockCount::Dynamic(expr) => {
                    let native = equals_slot(g, m.control[TB_NATIVE_SETTLE], 1);
                    let evaluate = g.b.new_block();
                    let host = g.b.new_block();
                    branch(g, native, evaluate, host);
                    g.b.switch_to_block(host);
                    put(g, m.control[TB_PHASE], 2 + 2 * i as u64);
                    put(g, m.control[TB_EXIT], TB_HOST);
                    g.b.seal_block(SIRTerminator::Return);
                    g.b.switch_to_block(evaluate);
                    let after = g.b.new_block();
                    settle_then(g, m, after);
                    g.b.switch_to_block(after);
                    let expr = expr.kernel().expect("lowered clock expression");
                    g.b.inline_execution_unit(&program.sir.processes[expr.entry]);
                    if !expr.writes.is_empty() {
                        let dirty = g.constant(1, 8);
                        g.store(slots.dirty, 8, dirty);
                    }
                    let value =
                        g.load_at(expr.result.address, expr.result.byte_offset * 8, expr.width);
                    let result = g.b.alloc_bit(64, false);
                    if expr.width > 64 {
                        g.b.emit(SIRInstruction::Slice(result, value, 0, 64));
                    } else {
                        g.b.emit(SIRInstruction::Unary(
                            result,
                            celox_design::UnaryOp::Ident,
                            value,
                        ));
                    }
                    result
                }
            };
            let zero = g.equals(count, 0);
            let wait = g.b.new_block();
            branch(g, zero, run[i], wait);
            g.b.switch_to_block(wait);
            g.store(p[1], 64, count);
            put(g, p[0], 1);
            put(g, p[2], clock as u64);
            wait_on_clock(g, m, clock);
            jump(g, save[i]);
        }
        KernelRequest::Statement(site) => match statements[*site] {
            SemanticStatement::Assert {
                expr,
                site_id,
                continue_on_fail,
                message,
                ..
            } if message_args(message).is_some() => {
                // The host records passing assertions itself, and
                // settles when observers watch the design.
                let record = equals_slot(g, m.control[TB_RECORD], 0);
                let native = equals_slot(g, m.control[TB_NATIVE_SETTLE], 1);
                let native = g.binary(1, record, BinaryOp::And, native);
                let evaluate_block = g.b.new_block();
                let host = g.b.new_block();
                branch(g, native, evaluate_block, host);
                g.b.switch_to_block(host);
                host_statement(g, m, i);
                g.b.switch_to_block(evaluate_block);
                let mut reads = dut_reads(program, expr);
                if let Some(args) = message_args(message) {
                    for arg in args {
                        reads.extend(dut_reads(program, &arg.expr));
                    }
                }
                settle_for(g, m, cones, deferred, reads, message.is_some(), finish);
                let value = evaluate(g, program, slots, expr);
                let passed = truth(g, value);
                // A passing assertion does not format its message,
                // but what its arguments write still happens.
                let effects: Vec<_> = message_args(message)
                    .unwrap()
                    .iter()
                    .filter(|arg| arg.expr.writes_state())
                    .collect();
                let pass = if effects.is_empty() {
                    run[i]
                } else {
                    g.b.new_block()
                };
                let failed = g.b.new_block();
                branch(g, passed, pass, failed);
                if !effects.is_empty() {
                    g.b.switch_to_block(pass);
                    for arg in effects {
                        evaluate(g, program, slots, &arg.expr);
                    }
                    jump(g, run[i]);
                }
                g.b.switch_to_block(failed);
                let site = &program.runtime.runtime_schema.runtime_event_sites[*site_id as usize];
                let widths = site.arg_widths.clone();
                let args = message_args(message)
                    .unwrap()
                    .iter()
                    .zip(widths)
                    .map(|(arg, width)| {
                        let value = evaluate(g, program, slots, &arg.expr);
                        resize(g, value, width.max(1))
                    })
                    .collect();
                g.b.emit(SIRInstruction::RuntimeEvent {
                    site_id: *site_id,
                    args,
                });
                if *continue_on_fail {
                    resume_after_events(g, m, i, run[i], capacity);
                } else {
                    mark(g, m);
                    put(g, m.control[TB_PHASE], 2 + 2 * i as u64);
                    put(g, m.control[TB_EXIT], TB_FAILED);
                    g.b.seal_block(SIRTerminator::Return);
                }
            }
            SemanticStatement::Display { message, .. }
                if let Some(&(_, site_id)) =
                    kernel.display_sites.iter().find(|(s, _)| s == site) =>
            {
                let native = equals_slot(g, m.control[TB_NATIVE_SETTLE], 1);
                let evaluate_block = g.b.new_block();
                let host = g.b.new_block();
                branch(g, native, evaluate_block, host);
                g.b.switch_to_block(host);
                host_statement(g, m, i);
                g.b.switch_to_block(evaluate_block);
                settle_for(g, m, cones, deferred, vec![], true, finish);
                let site = &program.runtime.runtime_schema.runtime_event_sites[site_id as usize];
                let widths = site.arg_widths.clone();
                let args = message_args(message)
                    .unwrap_or_default()
                    .iter()
                    .zip(widths)
                    .map(|(arg, width)| {
                        let value = evaluate(g, program, slots, &arg.expr);
                        resize(g, value, width.max(1))
                    })
                    .collect();
                g.b.emit(SIRInstruction::RuntimeEvent { site_id, args });
                resume_after_events(g, m, i, run[i], capacity);
            }
            SemanticStatement::Finish => {
                // A lone process on one clock ends the run; any
                // other process stops every process at this time.
                if m.processes.len() == 1 && m.clocks.len() <= 1 {
                    jump(g, finished);
                } else {
                    put(g, m.control[TB_STOPPED], 1);
                    put(g, p[0], 2);
                    jump(g, save[i]);
                }
            }
            SemanticStatement::ResetAssert {
                reset_signal,
                clock_event,
                duration,
                assert_value,
                ..
            } => {
                let clock = m.events.iter().position(|a| a == clock_event).unwrap();
                let count = match duration {
                    ClockCount::Static(n) => g.constant((*n).max(1), 64),
                    ClockCount::Dynamic(expr) => {
                        let native = equals_slot(g, m.control[TB_NATIVE_SETTLE], 1);
                        let evaluate_block = g.b.new_block();
                        let host = g.b.new_block();
                        branch(g, native, evaluate_block, host);
                        g.b.switch_to_block(host);
                        host_statement(g, m, i);
                        g.b.switch_to_block(evaluate_block);
                        settle_for(g, m, cones, deferred, vec![], true, finish);
                        let value = evaluate(g, program, slots, expr);
                        let value = two_state(g, value);
                        let count = resize(g, value, 64);
                        let zero = g.equals(count, 0);
                        let one = g.constant(1, 64);
                        choose(g, zero, one, count)
                    }
                };
                write_reset(g, reset_signal, *assert_value);
                g.store(p[1], 64, count);
                put(g, p[0], 1);
                put(g, p[2], clock as u64);
                put(g, p[4], request_index as u64 + 1);
                wait_on_clock(g, m, clock);
                jump(g, save[i]);
            }
            _ => host_statement(g, m, i),
        },
        _ => host_statement(g, m, i),
    }
}
