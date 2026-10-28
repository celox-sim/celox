//! Veryl TB continuations and clock progress in the shared SIR backend.
use super::*;
use celox_sir::BlockId;
use celox_testbench::*;

type Meta = TestbenchScheduler<AbsoluteAddr>;

pub(super) fn attach(program: &mut LaidOutProgram) {
    let Some(tb) = program.runtime.testbench.clone() else {
        return;
    };
    let Some(mut meta) = tb.scheduler().cloned() else {
        return;
    };
    let Some(schema) = program.runtime.runtime_schema.scheduler.clone() else {
        return;
    };
    let unit = generate(program, &tb, &meta, &schema.slots);
    meta.entry = Some(program.sir.processes.len());
    program.sir.processes.push(unit);
    program
        .runtime
        .testbench
        .as_mut()
        .unwrap()
        .set_scheduler(meta);
}

fn put(g: &mut Generator<'_>, a: AbsoluteAddr, value: u64) {
    let value = g.constant(value, 64);
    g.store(a, 64, value);
}
fn jump(g: &mut Generator<'_>, to: BlockId) {
    g.b.seal_block(SIRTerminator::Jump(to, vec![]));
}
fn branch(g: &mut Generator<'_>, cond: RegisterId, yes: BlockId, no: BlockId) {
    g.b.seal_block(SIRTerminator::Branch {
        cond,
        true_block: (yes, vec![]),
        false_block: (no, vec![]),
    });
}
fn dispatch(
    g: &mut Generator<'_>,
    selector: RegisterId,
    cases: Vec<SIRSwitchCase>,
    default: BlockId,
) {
    for case in cases {
        let constant = case.value.to_u64_digits().first().copied().unwrap_or(0);
        let matched = g.equals(selector, constant);
        let next = g.b.new_block();
        branch(g, matched, case.target, next);
        g.b.switch_to_block(next);
    }
    jump(g, default);
}
// TBs can have more continuation sites than the timed scheduler's 8-bit
// continuation selector. Keep their dispatch in the TB's own 64-bit slot.
fn settle_then(g: &mut Generator<'_>, m: &Meta, target: BlockId) {
    let index = match g.continuations.iter().position(|&block| block == target) {
        Some(i) => i,
        None => {
            g.continuations.push(target);
            g.continuations.len() - 1
        }
    };
    put(g, m.control[TB_SETTLE_CONT], index as u64);
    jump(g, g.comb);
}
fn emit_settle(g: &mut Generator<'_>, m: &Meta, units: &[Unit], finish: BlockId, invalid: BlockId) {
    g.b.switch_to_block(g.comb);
    let dirty = g.load(g.slots.dirty, 8);
    let clean = g.equals(dirty, 0);
    let evaluate = g.b.new_block();
    let settled = g.b.new_block();
    branch(g, clean, settled, evaluate);
    g.b.switch_to_block(evaluate);
    for unit in units {
        g.inline(unit, finish);
    }
    let zero = g.constant(0, 8);
    g.store(g.slots.dirty, 8, zero);
    jump(g, settled);
    g.b.switch_to_block(settled);
    let continuation = g.load(m.control[TB_SETTLE_CONT], 64);
    let cases = g
        .continuations
        .iter()
        .enumerate()
        .map(|(i, &target)| SIRSwitchCase {
            value: (i as u64).into(),
            target,
        })
        .collect();
    dispatch(g, continuation, cases, invalid);
}
fn choose(g: &mut Generator<'_>, cond: RegisterId, yes: RegisterId, no: RegisterId) -> RegisterId {
    let r = g.b.alloc_bit(64, false);
    g.b.emit(SIRInstruction::Mux(r, cond, yes, no));
    r
}
fn equals_slot(g: &mut Generator<'_>, a: AbsoluteAddr, value: u64) -> RegisterId {
    let r = g.load(a, 64);
    g.equals(r, value)
}
fn equals_slot_width(
    g: &mut Generator<'_>,
    a: AbsoluteAddr,
    width: usize,
    value: u64,
) -> RegisterId {
    let r = g.load(a, width);
    g.equals(r, value)
}
fn copy(g: &mut Generator<'_>, program: &LaidOutProgram, from: AbsoluteAddr, to: AbsoluteAddr) {
    let info = &program.design.state_objects[&from];
    let elements = info.array_dims.iter().product::<usize>().max(1);
    let width = info.width / elements;
    for index in 0..elements {
        let r = if info.is_4state {
            g.b.alloc_logic(width)
        } else {
            g.b.alloc_bit(width, false)
        };
        g.b.emit(SIRInstruction::Load(
            r,
            stable(from),
            SIROffset::Static(index * width),
            width,
        ));
        g.b.emit(SIRInstruction::Store(
            stable(to),
            SIROffset::Static(index * width),
            width,
            r,
            vec![],
            vec![],
        ));
    }
}
fn sites<'a>(
    body: &'a [SemanticStatement<AbsoluteAddr>],
    out: &mut Vec<&'a SemanticStatement<AbsoluteAddr>>,
) {
    for s in body {
        out.push(s);
        match s {
            SemanticStatement::If {
                then_block,
                else_block,
                ..
            } => {
                sites(then_block, out);
                sites(else_block, out);
            }
            SemanticStatement::For { body, .. } => sites(body, out),
            _ => {}
        }
    }
}

fn generate(
    program: &LaidOutProgram,
    tb: &TestbenchProgram<AbsoluteAddr>,
    m: &Meta,
    slots: &SchedulerSlots<AbsoluteAddr>,
) -> Unit {
    let mut b = Builder::new();
    let comb = b.new_block();
    let reads = HashSet::default();
    let finish_sites = program
        .runtime
        .runtime_schema
        .runtime_event_sites
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s.kind, RuntimeEventKind::Finish | RuntimeEventKind::Stop))
        .map(|(i, _)| i as u32)
        .collect();
    let mut g = Generator {
        b,
        slots,
        finish_sites: &finish_sites,
        exits: vec![],
        settle_reads: Some(&reads),
        shared: program.shared_storage(),
        comb,
        continuations: vec![],
    };
    let initialize = g.b.new_block();
    let start = g.b.new_block();
    let edge = g.b.new_block();
    let after_edges = g.b.new_block();
    let end = g.b.new_block();
    let done = g.b.new_block();
    let limited = g.b.new_block();
    let overflow = g.b.new_block();
    let safe = g.b.new_block();
    let finish = g.b.new_block();
    let finished = g.b.new_block();
    let release_check: Vec<_> = m.processes.iter().map(|_| g.b.new_block()).collect();
    let run: Vec<_> = m.processes.iter().map(|_| g.b.new_block()).collect();
    let pick: Vec<_> = m.processes.iter().map(|_| g.b.new_block()).collect();
    let released: Vec<_> = m.processes.iter().map(|_| g.b.new_block()).collect();
    let save: Vec<_> = m.processes.iter().map(|_| g.b.new_block()).collect();
    let mut cases = vec![
        SIRSwitchCase {
            value: u64::MAX.into(),
            target: initialize,
        },
        SIRSwitchCase {
            value: 0u64.into(),
            target: start,
        },
        SIRSwitchCase {
            value: 1u64.into(),
            target: after_edges,
        },
    ];
    for (i, &r) in run.iter().enumerate() {
        cases.push(SIRSwitchCase {
            value: (2 + 2 * i as u64).into(),
            target: r,
        });
        cases.push(SIRSwitchCase {
            value: (3 + 2 * i as u64).into(),
            target: released[i],
        });
    }
    for (i, &target) in save.iter().enumerate() {
        cases.push(SIRSwitchCase {
            value: (2 + 2 * run.len() as u64 + i as u64).into(),
            target,
        });
    }
    let phase = g.load(m.control[TB_PHASE], 64);
    dispatch(&mut g, phase, cases, overflow);

    g.b.switch_to_block(initialize);
    for locals in &m.locals {
        for &(source, saved) in locals {
            copy(&mut g, program, source, saved);
        }
    }
    jump(&mut g, start);
    g.b.switch_to_block(start);
    let now = g.load(m.control[TB_NOW], 64);
    g.store(m.control[TB_REPORT_TIME], 64, now);
    for &(source, saved) in &m.baselines {
        copy(&mut g, program, source, saved);
    }
    jump(&mut g, release_check.first().copied().unwrap_or(edge));

    for (i, kernel) in tb.kernels().iter().enumerate() {
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
        branch(&mut g, release, wake_block, released[i]);
        g.b.switch_to_block(wake_block);
        put(&mut g, p[0], 0);
        let reset = g.load(p[4], 64);
        let has_reset = g.equals(reset, 0);
        let reset_host = g.b.new_block();
        branch(&mut g, has_reset, released[i], reset_host);
        g.b.switch_to_block(reset_host);
        put(&mut g, m.control[TB_CURRENT], i as u64);
        put(&mut g, m.control[TB_PHASE], 3 + 2 * i as u64);
        put(&mut g, m.control[TB_EXIT], TB_RESET);
        g.b.seal_block(SIRTerminator::Return);
        g.b.switch_to_block(released[i]);
        jump(
            &mut g,
            release_check.get(i + 1).copied().unwrap_or_else(|| pick[0]),
        );
        g.b.switch_to_block(pick[i]);
        let ready = equals_slot(&mut g, p[0], 0);
        let ticks = g.load(m.control[TB_TICKS], 64);
        let limit = g.load(m.control[TB_LIMIT], 64);
        let active = g.binary(1, ticks, BinaryOp::LtU, limit);
        let running = equals_slot(&mut g, m.control[TB_STOPPED], 0);
        let active = g.binary(1, active, BinaryOp::And, running);
        let ready = g.binary(1, ready, BinaryOp::And, active);
        let restore = g.b.new_block();
        branch(&mut g, ready, restore, next);
        g.b.switch_to_block(restore);
        for &(source, saved) in &m.locals[i] {
            copy(&mut g, program, saved, source);
        }
        jump(&mut g, run[i]);
        g.b.switch_to_block(run[i]);
        put(&mut g, m.control[TB_CURRENT], i as u64);
        g.b.inline_execution_unit(&program.sir.processes[kernel.entry]);
        let status = g.load(kernel.status, celox_design::PROCESS_STATUS_WIDTH);
        let is_done = g.equals(status, ProcessStatus::Done.code() as u64);
        let completed = g.b.new_block();
        let request = g.b.new_block();
        branch(&mut g, is_done, completed, request);
        g.b.switch_to_block(completed);
        put(&mut g, p[0], 2);
        jump(&mut g, save[i]);
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
        dispatch(&mut g, request_value, cases, overflow);
        let mut statements = vec![];
        sites(tb.processes().nth(i).unwrap(), &mut statements);
        for (request, block) in kernel.requests.iter().zip(requests) {
            g.b.switch_to_block(block);
            match request {
                KernelRequest::Settle => {
                    let native = equals_slot(&mut g, m.control[TB_NATIVE_SETTLE], 1);
                    let settle = g.b.new_block();
                    let host = g.b.new_block();
                    branch(&mut g, native, settle, host);
                    g.b.switch_to_block(host);
                    put(&mut g, m.control[TB_PHASE], 2 + 2 * i as u64);
                    put(&mut g, m.control[TB_EXIT], TB_HOST);
                    g.b.seal_block(SIRTerminator::Return);
                    g.b.switch_to_block(settle);
                    settle_then(&mut g, m, run[i]);
                }
                KernelRequest::Written { .. } => {
                    let dirty = g.constant(1, 8);
                    g.store(slots.dirty, 8, dirty);
                    jump(&mut g, run[i]);
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
                            let native = equals_slot(&mut g, m.control[TB_NATIVE_SETTLE], 1);
                            let evaluate = g.b.new_block();
                            let host = g.b.new_block();
                            branch(&mut g, native, evaluate, host);
                            g.b.switch_to_block(host);
                            put(&mut g, m.control[TB_PHASE], 2 + 2 * i as u64);
                            put(&mut g, m.control[TB_EXIT], TB_HOST);
                            g.b.seal_block(SIRTerminator::Return);
                            g.b.switch_to_block(evaluate);
                            let after = g.b.new_block();
                            settle_then(&mut g, m, after);
                            g.b.switch_to_block(after);
                            let expr = expr.kernel().expect("lowered clock expression");
                            g.b.inline_execution_unit(&program.sir.processes[expr.entry]);
                            if !expr.writes.is_empty() {
                                let dirty = g.constant(1, 8);
                                g.store(slots.dirty, 8, dirty);
                            }
                            let value = g.load_at(
                                expr.result.address,
                                expr.result.byte_offset * 8,
                                expr.width,
                            );
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
                    branch(&mut g, zero, run[i], wait);
                    g.b.switch_to_block(wait);
                    g.store(p[1], 64, count);
                    put(&mut g, p[0], 1);
                    put(&mut g, p[2], clock as u64);
                    let enabled = equals_slot(&mut g, m.clocks[clock][3], 0);
                    let activate = g.b.new_block();
                    branch(&mut g, enabled, activate, save[i]);
                    g.b.switch_to_block(activate);
                    put(&mut g, m.clocks[clock][3], 1);
                    let now = g.load(m.control[TB_NOW], 64);
                    g.store(m.clocks[clock][0], 64, now);
                    jump(&mut g, save[i]);
                }
                _ => {
                    put(&mut g, m.control[TB_PHASE], 2 + 2 * i as u64);
                    put(&mut g, m.control[TB_EXIT], TB_HOST);
                    g.b.seal_block(SIRTerminator::Return);
                }
            }
        }
        g.b.switch_to_block(save[i]);
        for &(source, saved) in &m.locals[i] {
            copy(&mut g, program, source, saved);
        }
        jump(&mut g, next);
    }

    // Edge execution yields to the runtime only when observers require it.
    // Its continuation is independent of TB process continuations.
    g.b.switch_to_block(edge);
    let host_edges = g.b.new_block();
    let schema = program.runtime.runtime_schema.scheduler.as_ref().unwrap();
    let positions: Option<Vec<_>> = m
        .events
        .iter()
        .map(|event| {
            schema.clocks.iter().position(|a| {
                program.layout().offsets.get(a) == program.layout().offsets.get(event)
            })
        })
        .collect();
    if let (Some(unit), Some(positions)) = (schema.unit, positions) {
        let mut allowed = equals_slot(&mut g, m.control[TB_NATIVE_EDGES], 1);
        let clean = equals_slot_width(&mut g, slots.dirty, 8, 0);
        allowed = g.binary(1, allowed, BinaryOp::And, clean);
        for &(source, saved) in &m.baselines {
            let before = g.load(saved, 1);
            let current = g.load(source, 1);
            let same = g.binary(1, before, BinaryOp::Eq, current);
            allowed = g.binary(1, allowed, BinaryOp::And, same);
        }
        let now = g.load(m.control[TB_NOW], 64);
        let last = g.equals(now, u64::MAX);
        let safe_time = g.equals(last, 0);
        allowed = g.binary(1, allowed, BinaryOp::And, safe_time);
        let native = g.b.new_block();
        branch(&mut g, allowed, native, host_edges);
        g.b.switch_to_block(native);
        let now = g.load(m.control[TB_NOW], 64);
        let ticks = g.load(m.control[TB_TICKS], 64);
        let limit = g.load(m.control[TB_LIMIT], 64);
        let active = g.binary(1, ticks, BinaryOp::LtU, limit);
        let running = equals_slot(&mut g, m.control[TB_STOPPED], 0);
        let active = g.binary(1, active, BinaryOp::And, running);
        let mut armed = g.constant(0, 64);
        let mut values = g.constant(0, 64);
        for (c, &bit) in m.clocks.iter().zip(&positions) {
            let enabled = equals_slot(&mut g, c[3], 1);
            let next = g.load(c[0], 64);
            let rise = g.binary(1, next, BinaryOp::Eq, now);
            let rise = g.binary(1, rise, BinaryOp::And, enabled);
            let rise = g.binary(1, rise, BinaryOp::And, active);
            let falling = equals_slot(&mut g, c[2], 1);
            let next = g.load(c[1], 64);
            let fall = g.binary(1, next, BinaryOp::Eq, now);
            let fall = g.binary(1, fall, BinaryOp::And, falling);
            let due = g.binary(1, rise, BinaryOp::Or, fall);
            let set = g.bit(due, bit);
            armed = g.binary(64, armed, BinaryOp::Or, set);
            let set = g.bit(rise, bit);
            values = g.binary(64, values, BinaryOp::Or, set);
            g.store_at(slots.clock_next, bit * 64, 64, now);
            let two = g.constant(2, 64);
            g.store_at(slots.clock_period, bit * 64, 64, two);
        }
        let any = g.equals(armed, 0);
        let invoke = g.b.new_block();
        branch(&mut g, any, host_edges, invoke);
        g.b.switch_to_block(invoke);
        g.store(slots.clock_armed, 64, armed);
        g.store(slots.clock_value, 64, values);
        g.store(slots.limit, 64, now);
        put(&mut g, slots.count, 0);
        put(&mut g, slots.ready, 0);
        let zero = g.constant(0, 8);
        g.store(slots.exit, 8, zero);
        g.store(slots.finish, 8, zero);
        g.b.inline_execution_unit(&program.sir.processes[unit]);
        let exit = g.load(slots.exit, 8);
        let limit = g.equals(exit, SchedulerExit::Limit as u64);
        let finished = g.equals(exit, SchedulerExit::Finish as u64);
        let ok = g.binary(1, limit, BinaryOp::Or, finished);
        branch(&mut g, ok, after_edges, overflow);
    } else {
        jump(&mut g, host_edges);
    }
    g.b.switch_to_block(host_edges);
    put(&mut g, m.control[TB_PHASE], 1);
    put(&mut g, m.control[TB_EXIT], TB_EDGES);
    g.b.seal_block(SIRTerminator::Return);
    g.b.switch_to_block(after_edges);
    let now = g.load(m.control[TB_NOW], 64);
    let ticks = g.load(m.control[TB_TICKS], 64);
    let limit = g.load(m.control[TB_LIMIT], 64);
    let active = g.binary(1, ticks, BinaryOp::LtU, limit);
    let running = equals_slot(&mut g, m.control[TB_STOPPED], 0);
    let active = g.binary(1, active, BinaryOp::And, running);
    let mut rising = g.constant(0, 1);
    for (clock, c) in m.clocks.iter().enumerate() {
        let enabled = equals_slot(&mut g, c[3], 1);
        let next = g.load(c[0], 64);
        let due = g.binary(1, next, BinaryOp::Eq, now);
        let due = g.binary(1, due, BinaryOp::And, enabled);
        let due = g.binary(1, due, BinaryOp::And, active);
        rising = g.binary(1, rising, BinaryOp::Or, due);
        let fall = g.load(c[1], 64);
        let falling = g.binary(1, fall, BinaryOp::Eq, now);
        let armed = g.load(c[2], 64);
        let not_due = g.equals(falling, 0);
        let zero = g.constant(0, 64);
        let remain = choose(&mut g, not_due, armed, zero);
        g.store(c[2], 64, remain);
        let fire = g.b.new_block();
        let skip = g.b.new_block();
        branch(&mut g, due, fire, skip);
        g.b.switch_to_block(fire);
        let period = g.constant(m.periods[clock], 64);
        let next = g.binary(64, now, BinaryOp::Add, period);
        let wraps = g.binary(1, next, BinaryOp::LtU, now);
        let valid = g.b.new_block();
        branch(&mut g, wraps, overflow, valid);
        g.b.switch_to_block(valid);
        g.store(c[0], 64, next);
        let half = g.constant(m.periods[clock] - m.periods[clock] / 2, 64);
        let fall = g.binary(64, now, BinaryOp::Add, half);
        g.store(c[1], 64, fall);
        put(&mut g, c[2], 1);
        for p in &m.processes {
            let waiting = equals_slot(&mut g, p[0], 1);
            let this = equals_slot(&mut g, p[2], clock as u64);
            let waiting = g.binary(1, waiting, BinaryOp::And, this);
            let decrement = g.b.new_block();
            let after = g.b.new_block();
            branch(&mut g, waiting, decrement, after);
            g.b.switch_to_block(decrement);
            let left = g.load(p[1], 64);
            let one = g.constant(1, 64);
            let left = g.binary(64, left, BinaryOp::Sub, one);
            g.store(p[1], 64, left);
            let last = g.equals(left, 0);
            let release = g.b.new_block();
            branch(&mut g, last, release, after);
            g.b.switch_to_block(release);
            put(&mut g, p[0], 3);
            g.store(p[3], 64, next);
            jump(&mut g, after);
            g.b.switch_to_block(after);
        }
        jump(&mut g, skip);
        g.b.switch_to_block(skip);
    }
    let one = g.constant(1, 64);
    let zero = g.constant(0, 64);
    let add = choose(&mut g, rising, one, zero);
    let ticks = g.binary(64, ticks, BinaryOp::Add, add);
    g.store(m.control[TB_TICKS], 64, ticks);
    jump(&mut g, end);
    g.b.switch_to_block(end);
    let stopped = equals_slot(&mut g, m.control[TB_STOPPED], 1);
    let check_done = g.b.new_block();
    branch(&mut g, stopped, finished, check_done);
    g.b.switch_to_block(check_done);
    let mut complete = g.constant(1, 1);
    for p in &m.processes {
        let is_done = equals_slot(&mut g, p[0], 2);
        complete = g.binary(1, complete, BinaryOp::And, is_done);
    }
    let advance = g.b.new_block();
    branch(&mut g, complete, done, advance);
    g.b.switch_to_block(advance);
    let ticks = g.load(m.control[TB_TICKS], 64);
    let limit = g.load(m.control[TB_LIMIT], 64);
    let active = g.binary(1, ticks, BinaryOp::LtU, limit);
    let running = equals_slot(&mut g, m.control[TB_STOPPED], 0);
    let active = g.binary(1, active, BinaryOp::And, running);
    let mut next = g.constant(u64::MAX, 64);
    let mut pending = g.constant(0, 1);
    for c in &m.clocks {
        let enabled = equals_slot(&mut g, c[3], 1);
        let enabled = g.binary(1, enabled, BinaryOp::And, active);
        let rise = g.load(c[0], 64);
        let earlier = g.binary(1, rise, BinaryOp::LtU, next);
        let choose_rise = g.binary(1, earlier, BinaryOp::And, enabled);
        next = choose(&mut g, choose_rise, rise, next);
        pending = g.binary(1, pending, BinaryOp::Or, enabled);
        let armed = equals_slot(&mut g, c[2], 1);
        let fall = g.load(c[1], 64);
        let earlier = g.binary(1, fall, BinaryOp::LtU, next);
        let choose_fall = g.binary(1, earlier, BinaryOp::And, armed);
        next = choose(&mut g, choose_fall, fall, next);
        pending = g.binary(1, pending, BinaryOp::Or, armed);
    }
    for p in &m.processes {
        let releasing = equals_slot(&mut g, p[0], 3);
        let wake = g.load(p[3], 64);
        let earlier = g.binary(1, wake, BinaryOp::LtU, next);
        let take = g.binary(1, releasing, BinaryOp::And, earlier);
        next = choose(&mut g, take, wake, next);
        pending = g.binary(1, pending, BinaryOp::Or, releasing);
    }
    let advance = g.b.new_block();
    branch(&mut g, pending, advance, limited);
    g.b.switch_to_block(advance);
    g.store(m.control[TB_NOW], 64, next);
    put(&mut g, m.control[TB_PHASE], 0);
    let budget = g.load(m.control[TB_BUDGET], 64);
    let one = g.constant(1, 64);
    let budget = g.binary(64, budget, BinaryOp::Sub, one);
    g.store(m.control[TB_BUDGET], 64, budget);
    let exhausted = g.equals(budget, 0);
    branch(&mut g, exhausted, safe, start);
    for (block, code) in [
        (done, TB_DONE),
        (finished, TB_FINISHED),
        (limited, TB_LIMITED),
        (overflow, TB_OVERFLOW),
        (safe, TB_SAFEPOINT),
        (finish, TB_SAFEPOINT),
    ] {
        g.b.switch_to_block(block);
        put(&mut g, m.control[TB_EXIT], code);
        g.b.seal_block(SIRTerminator::Return);
    }
    emit_settle(&mut g, m, &program.sir.eval_comb, finish, overflow);
    let mut unit = g.finish();
    let mut next = unit.register_map.keys().map(|r| r.0).max().unwrap_or(0) + 1;
    for block in unit.blocks.values_mut() {
        let old = std::mem::take(&mut block.instructions);
        for instruction in old {
            let event = matches!(instruction, SIRInstruction::RuntimeEvent { .. });
            block.instructions.push(instruction);
            if event {
                let one = RegisterId(next);
                next += 1;
                unit.register_map.insert(
                    one,
                    celox_sir::RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                );
                block
                    .instructions
                    .push(SIRInstruction::Imm(one, SIRValue::new(1u64)));
                block.instructions.push(SIRInstruction::Store(
                    stable(m.control[TB_BUDGET]),
                    SIROffset::Static(0),
                    64,
                    one,
                    vec![],
                    vec![],
                ));
            }
        }
    }
    unit
}
