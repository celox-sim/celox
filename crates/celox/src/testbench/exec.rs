//! Executing testbench statements and concurrent testbench processes.

use super::*;

pub(super) struct ProcessWait<Event> {
    pub(super) clock: Event,
    pub(super) remaining: u64,
    pub(super) period: u64,
    pub(super) resume_at: Option<u64>,
    pub(super) reset: Option<(SignalRef, Option<Event>, u8)>,
}

pub(super) struct ProcessClock<Event> {
    pub(super) event: Event,
    pub(super) period: u64,
    pub(super) next_edge: u64,
    pub(super) falling_edge: Option<u64>,
}

/// Whether some process waits on `clock`: the generator's enable.
pub(super) fn waited_on<Event: celox_runtime::backend::EventHandle>(
    waits: &[Option<ProcessWait<Event>>],
    clock: &ProcessClock<Event>,
) -> bool {
    waits.iter().any(|wait| {
        wait.as_ref()
            .is_some_and(|wait| wait.remaining != 0 && wait.clock.id() == clock.event.id())
    })
}

// Clock generators start low even when the design uses four-state storage.
// Otherwise X's nonzero payload can make the first scheduled 1 look like a
// steady high to the edge detector, losing the first clock/reset cycle.
// Returns how many distinct clocks the testbench has, declared or waited
// on. A declared generator nobody waits on stays low and drives nothing,
// but it still keeps the timed scheduler, which reports each process's
// time in units of the clock periods rather than in cycles.
pub(super) fn initialize_process_clocks<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
) -> usize {
    fn referenced<B: SimBackend>(statements: &[TestbenchStatement<B>], clocks: &mut Vec<B::Event>) {
        for statement in statements {
            match statement {
                GenericTestbenchStatement::ClockNext { clock_event, .. }
                | GenericTestbenchStatement::ResetAssert { clock_event, .. } => {
                    clocks.push(*clock_event);
                }
                GenericTestbenchStatement::If {
                    then_block,
                    else_block,
                    ..
                } => {
                    referenced::<B>(then_block, clocks);
                    referenced::<B>(else_block, clocks);
                }
                GenericTestbenchStatement::For { body, .. } => referenced::<B>(body, clocks),
                _ => {}
            }
        }
    }
    let mut waited: Vec<B::Event> = Vec::new();
    referenced::<B>(testbench.statements(), &mut waited);
    let mut unique: Vec<B::Event> = Vec::with_capacity(waited.len());
    for event in waited {
        if unique.iter().all(|known| known.id() != event.id()) {
            unique.push(event);
        }
    }
    for clock in testbench.clocks() {
        if unique.iter().all(|known| known.id() != clock.event.id()) {
            unique.push(clock.event);
        }
    }
    for &event in &unique {
        let signal = sim.backend.resolve_signal(&event.addr());
        sim_set_u64(sim, signal, 0);
    }
    unique.len()
}

pub(super) fn exec_testbench<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    ctx: &mut DetailedExecContext,
) -> ExecResult {
    let blocks: Vec<_> = testbench.processes().collect();
    let clocks = initialize_process_clocks(sim, testbench);
    // A finished kernel returns without passing a collection point, so
    // collect its temporaries here; no process is suspended anymore.
    if let Some(result) = compiled::run(sim, testbench, ctx, clocks) {
        sim.heap_safepoint();
        return result;
    }
    // One process on one clock spends time only in that clock's periods, so
    // stepping them in place is the whole schedule.
    if blocks.is_empty() {
        return ExecResult::Continue;
    }
    if blocks.len() == 1 && clocks <= 1 {
        let mut process = match native::Process::<B>::compile(blocks[0], &testbench.kernels()[0]) {
            Ok(process) => process,
            Err(error) => return ExecResult::Fail(format!("testbench compilation: {error}")),
        };
        let result = match process.resume(sim, ctx, &mut None, &mut Vec::new(), false) {
            Poll::Ready(result) => result,
            Poll::Pending => unreachable!("synchronous clock execution cannot suspend"),
        };
        sim.heap_safepoint();
        return result;
    }
    // Suspended processes keep their temporaries outside state memory, so
    // only the scheduler loop collects, with those temporaries as roots.
    let blocked = std::mem::replace(&mut sim.heap.safepoints_blocked, true);
    let result = exec_processes(sim, testbench, ctx, &blocks);
    sim.heap.safepoints_blocked = blocked;
    result
}

pub(super) fn exec_processes<B: SimBackend>(
    sim: &mut Simulator<B>,
    testbench: &CompiledTestbench<B>,
    ctx: &mut DetailedExecContext,
    blocks: &[&[TestbenchStatement<B>]],
) -> ExecResult {
    // The compiler's function slots are shared by inline call sites. Keep
    // only these temporaries private; design variables remain shared between
    // initial processes and all RTL. Capture masks as well as payloads.
    let initial_locals: Vec<_> = testbench
        .private_signals()
        .iter()
        .map(|&signal| sim.backend.get_four_state(signal))
        .collect();
    let mut locals = vec![initial_locals; blocks.len()];
    let mut waits: Vec<Option<ProcessWait<B::Event>>> = blocks.iter().map(|_| None).collect();
    let mut reset_edges = Vec::new();
    let mut processes = match blocks
        .iter()
        .zip(testbench.kernels())
        .map(|(block, kernel)| native::Process::<B>::compile(block, kernel).map(Some))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(processes) => processes,
        Err(error) => return ExecResult::Fail(format!("testbench compilation: {error}")),
    };
    // A clock generator joins when a process first waits on it, and rises
    // only while some process does.
    let mut clocks: Vec<ProcessClock<B::Event>> = Vec::new();
    // Capture the baseline before runnable processes drive reset. Reset
    // assertions are signal writes AND events at this timestamp, even if
    // their clock's next edge is still in the future. Rebasing after the
    // writes would silently absorb the asynchronous transition.
    {
        if sim.dirty
            && let Err(error) = settle_testbench(sim)
        {
            return ExecResult::Fail(format!("eval_comb: {error}"));
        }
        let mut state = sim
            .component_simulation
            .take()
            .unwrap_or_else(|| Box::new(crate::simulation::simulation_state(sim)));
        state.synchronize_event_values(&sim.backend);
        sim.component_simulation = Some(state);
    }
    let clock_batch = clock_batch::ClockBatch::new(sim);
    let mut now = 0;
    // Reuse per-slot batches instead of allocating on each rising and falling
    // edge; their capacities can grow when a wait introduces another clock.
    let mut falling = Vec::with_capacity(clocks.len());
    let mut fired = Vec::with_capacity(clocks.len());
    let mut resets = Vec::with_capacity(waits.len());
    let mut events = Vec::with_capacity(clocks.len() + waits.len());
    let mut moves = Vec::with_capacity(clocks.len());
    loop {
        ctx.scheduler_time = Some(now);
        let draining = ctx.at_tick_limit();
        if draining {
            ctx.tick_limit_reached = true;
            // Stop starting work, but finish edges/releases already scheduled
            // by the final permitted tick before returning the simulator state.
            if clocks.iter().all(|clock| clock.falling_edge.is_none())
                && waits
                    .iter()
                    .all(|wait| wait.as_ref().is_none_or(|wait| wait.resume_at.is_none()))
            {
                return ExecResult::Finished;
            }
        }
        // A waiter resumes at the end of its last cycle. Collect all edge
        // polarities for this timestamp before evaluating clocked logic.
        falling.clear();
        falling.extend(clocks.iter_mut().filter_map(|clock| {
            if clock.falling_edge == Some(now) {
                clock.falling_edge = None;
                Some(clock.event)
            } else {
                None
            }
        }));
        for wait in &mut waits {
            if let Some(pending) = wait.as_ref()
                && pending.resume_at == Some(now)
            {
                if let Some((signal, event, value)) = pending.reset {
                    sim_set_u64(sim, signal, value.into());
                    if let Some(event) = event {
                        reset_edges.push((event, value));
                    }
                }
                *wait = None;
            }
        }
        // Resume ready processes in declaration order until their next wait,
        // completion, or a global stop. Each native call returns before another
        // process or a clock domain can run.
        let mut stopped = false;
        for ((process, wait), locals) in processes.iter_mut().zip(&mut waits).zip(&mut locals) {
            if draining || wait.is_some() {
                continue;
            }
            let Some(executable) = process else { continue };
            for (&signal, (value, mask)) in testbench.private_signals().iter().zip(locals.iter()) {
                write_testbench_signal(sim, signal, |sim| {
                    sim.set_four_state(signal, value.clone(), mask.clone());
                });
            }
            let result = executable.resume(sim, ctx, wait, &mut reset_edges, true);
            for (&signal, value) in testbench.private_signals().iter().zip(locals.iter_mut()) {
                *value = sim.backend.get_four_state(signal);
            }
            match result {
                Poll::Ready(ExecResult::Continue) => *process = None,
                Poll::Ready(ExecResult::Finished) => {
                    stopped = true;
                    break;
                }
                Poll::Ready(result) => return result,
                Poll::Pending => debug_assert!(wait.is_some()),
            }
        }
        // No process is running, so every string handle is in state memory or
        // in a suspended process's saved temporaries.
        collect_testbench_heap(sim, testbench.private_signals(), &locals);
        for wait in &waits {
            if let Some(wait) = wait.as_ref()
                && wait.remaining != 0
            {
                if let Some(clock) = clocks
                    .iter_mut()
                    .find(|clock| clock.event.id() == wait.clock.id())
                {
                    clock.next_edge = clock.next_edge.max(now);
                } else {
                    clocks.push(ProcessClock {
                        event: wait.clock,
                        period: wait.period.max(2),
                        next_edge: now,
                        falling_edge: None,
                    });
                }
            }
        }
        fired.clear();
        fired.extend(
            clocks
                .iter()
                .filter(|clock| {
                    !draining && !stopped && clock.next_edge == now && waited_on(&waits, clock)
                })
                .map(|clock| clock.event),
        );
        {
            resets.clear();
            resets.extend(waits.iter().filter_map(|wait| {
                let wait = wait.as_ref()?;
                wait.reset.map(|(_, event, _)| (wait.clock, event))
            }));
            events.clear();
            events.extend(
                falling
                    .iter()
                    .map(|&event| (event, 0))
                    .chain(fired.iter().map(|&event| (event, 1)))
                    .chain(reset_edges.drain(..)),
            );
            let result = step_process_events(
                sim,
                &events,
                &resets,
                now,
                !fired.is_empty(),
                ctx,
                &mut moves,
                clock_batch.as_ref(),
            );
            if !matches!(result, ExecResult::Continue) {
                return result;
            }
            for clock in &mut clocks {
                if fired.iter().any(|event| event.id() == clock.event.id()) {
                    let Some(next) = now.checked_add(clock.period) else {
                        return ExecResult::Fail("testbench clock time overflow".into());
                    };
                    clock.next_edge = next;
                    clock.falling_edge = Some(now + clock.period - clock.period / 2);
                }
            }
            for wait in &mut waits {
                if let Some(wait) = wait.as_mut()
                    && wait.remaining != 0
                    && let Some(clock) = clocks
                        .iter()
                        .find(|clock| clock.event.id() == wait.clock.id())
                    && fired.iter().any(|event| event.id() == wait.clock.id())
                {
                    wait.remaining -= 1;
                    if wait.remaining == 0 {
                        wait.resume_at = Some(clock.next_edge);
                    }
                }
            }
        }
        if stopped {
            return ExecResult::Finished;
        }
        if processes.iter().all(Option::is_none) {
            return ExecResult::Continue;
        }
        let next_edge = clocks
            .iter()
            .filter(|clock| !ctx.at_tick_limit() && waited_on(&waits, clock))
            .map(|clock| clock.next_edge)
            .min();
        let next_falling = clocks.iter().filter_map(|clock| clock.falling_edge).min();
        let next_resume = waits
            .iter()
            .filter_map(|wait| wait.as_ref().and_then(|wait| wait.resume_at))
            .min();
        if let Some(next) = [next_edge, next_falling, next_resume]
            .into_iter()
            .flatten()
            .min()
        {
            now = next;
        } else if ctx.at_tick_limit() {
            ctx.tick_limit_reached = true;
            return ExecResult::Finished;
        } else {
            unreachable!("a live process has a pending wait");
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn step_process_events<B: SimBackend>(
    sim: &mut Simulator<B>,
    events: &[(B::Event, u8)],
    resets: &[(B::Event, Option<B::Event>)],
    time: u64,
    rising: bool,
    ctx: &mut DetailedExecContext,
    moves: &mut Vec<(B::Event, celox_runtime::Edge)>,
    clock_batch: Option<&clock_batch::ClockBatch>,
) -> ExecResult {
    let mut state = sim
        .component_simulation
        .take()
        .unwrap_or_else(|| Box::new(crate::simulation::simulation_state(sim)));
    let edges = sim.event_edges();
    let schema = &sim.program.runtime_schema;
    // Plain RTL needs simultaneous edge evaluation, not the timed process
    // queue. The edge engine already skips inactive clock edges that cannot
    // run a domain. Keep reset transitions on the queue: reset_gen writes
    // their levels before submitting them, so its saved baseline matters.
    // Latches need inactive phases settled before the next edge. VCD needs
    // the queue's per-timestamp publication, even when no domain fires.
    // A TB assignment can change an unrelated event (including an async
    // reset). Preserve the queue's old baseline until it detects that change.
    // Dirty combinational logic may propagate such a change, so it also takes
    // the queue path even when no event signal has changed yet.
    let direct_edges = !events.is_empty()
        && !sim.dirty
        && !state.has_unsampled_event_changes(&sim.backend)
        && sim.vcd_writer.is_none()
        && schema.persistent_comb_state.is_empty()
        && sim.components.is_empty()
        && !sim.schedule.timed
        && schema.time_slots.is_empty()
        && schema.watches.is_empty()
        && schema.comb_observers.is_empty()
        && schema.sample_shadows.is_empty()
        && events.iter().all(|(event, _)| {
            matches!(
                edges[event.id()].kind,
                Some(
                    celox_design::DomainKind::ClockPosedge | celox_design::DomainKind::ClockNegedge
                )
            )
        });
    if !direct_edges
        && sim.dirty
        && let Err(error) = settle_testbench(sim)
    {
        sim.component_simulation = Some(state);
        return ExecResult::Fail(format!("eval_comb: {error}"));
    }
    for (clock, reset) in resets {
        sim.components
            .begin_reset_clock_cycles(clock.id(), reset.map(|event| event.id()));
    }
    // Submit all simultaneous edges before stepping: every clocked process
    // samples pre-edge values, before any nonblocking assignment is committed.
    let result = if direct_edges {
        moves.clear();
        moves.extend(events.iter().map(|&(event, value)| {
            (
                event,
                if value == 0 {
                    celox_runtime::Edge::Falling
                } else {
                    celox_runtime::Edge::Rising
                },
            )
        }));
        let result = clock_batch
            .and_then(|batch| batch.run(sim, events, time))
            .unwrap_or_else(|| sim.edges(moves).map_err(|error| error.to_string()))
            .map(|()| None);
        // A later reset can return to the queue. Its pre-write baseline must
        // include every clock and derived-domain update performed here.
        state.synchronize_event_values(&sim.backend);
        result
    } else {
        for &(event, value) in events {
            state.schedule(
                event,
                sim.backend.resolve_signal(&event.addr()),
                time,
                value,
            );
        }
        if events.is_empty() {
            state.settle_at(sim, time)
        } else {
            state.step(sim)
        }
        .map_err(|error| error.to_string())
    };
    sim.components.end_reset_cycles();
    sim.component_simulation = Some(state);
    if rising {
        ctx.current_time = ctx.current_time.saturating_add(1);
    }
    let drained = drain_runtime_assertions(sim, ctx, None);
    if let Some(message) = drained.fatal_message {
        return ExecResult::Fail(message_diagnostic(&message));
    }
    if let Err(error) = result {
        return ExecResult::Fail(error.to_string());
    }
    if drained.finished || sim.components.finish_requested() {
        return ExecResult::Finished;
    }
    ExecResult::Continue
}

pub(super) fn exec_one_detailed<B: SimBackend>(
    sim: &mut Simulator<B>,
    stmt: &TestbenchStatement<B>,
    ctx: &mut DetailedExecContext,
) -> ExecResult {
    let tick_limit_reached = |ctx: &mut DetailedExecContext| {
        let reached = ctx
            .tick_limit
            .is_some_and(|limit| ctx.current_time >= limit);
        if reached {
            ctx.tick_limit_reached = true;
        }
        reached
    };
    if tick_limit_reached(ctx) {
        return ExecResult::Finished;
    }
    match stmt {
        GenericTestbenchStatement::ClockNext {
            clock_event, count, ..
        } => match eval_clock_count(sim, count) {
            Ok(n) => {
                let progress_every = sim.diagnostics.testbench_progress_every;
                let mut remaining = n;
                while remaining != 0 {
                    if tick_limit_reached(ctx) {
                        return ExecResult::Finished;
                    }
                    let mut batch = remaining;
                    if let Some(limit) = ctx.tick_limit {
                        batch = batch.min(limit.saturating_sub(ctx.current_time));
                    }
                    if let Some(every) = progress_every.filter(|every| *every != 0) {
                        batch = batch.min(every - ctx.current_time % every);
                    }
                    let (completed, result) = if sim.components.has_scheduled_components() {
                        (1, tick_component_clock(sim, *clock_event, ctx.current_time))
                    } else {
                        let (completed, result) = clock_periods(sim, *clock_event, batch);
                        (completed, result.map_err(|error| error.to_string()))
                    };
                    if completed == 0 || completed > batch {
                        return ExecResult::Fail(
                            "backend made invalid progress in a deferred tick batch".into(),
                        );
                    }
                    ctx.current_time = ctx.current_time.saturating_add(completed);
                    remaining -= completed;
                    if let Err(e) = result {
                        let drained = drain_runtime_assertions(sim, ctx, None);
                        if let Some(message) = drained.fatal_message {
                            return ExecResult::Fail(message_diagnostic(&message));
                        }
                        return ExecResult::Fail(e);
                    }
                    if let Some(every) = progress_every
                        && every != 0
                        && ctx.current_time.is_multiple_of(every)
                    {
                        tracing::debug!("[testbench-progress] tick={}", ctx.current_time);
                    }
                    let drained = drain_runtime_assertions(sim, ctx, None);
                    if let Some(message) = drained.fatal_message {
                        return ExecResult::Fail(message_diagnostic(&message));
                    }
                    if drained.finished || sim.components.finish_requested() {
                        return ExecResult::Finished;
                    }
                }
                ExecResult::Continue
            }
            Err(error) => ExecResult::Fail(error.to_string()),
        },
        GenericTestbenchStatement::ResetAssert {
            reset_signal,
            reset_event,
            clock_event,
            duration,
            assert_value,
            deassert_value,
            ..
        } => match eval_clock_count(sim, duration) {
            Ok(duration) => {
                let duration = duration.max(1);
                sim_set_u64(sim, *reset_signal, (*assert_value).into());
                let mut remaining = duration;
                while remaining != 0 {
                    if tick_limit_reached(ctx) {
                        return ExecResult::Finished;
                    }
                    let mut batch = remaining;
                    if let Some(limit) = ctx.tick_limit {
                        batch = batch.min(limit.saturating_sub(ctx.current_time));
                    }
                    let component_aware = sim.components.has_scheduled_components();
                    let (completed, result) = if component_aware {
                        (
                            1,
                            tick_component_reset(
                                sim,
                                *reset_event,
                                *clock_event,
                                ctx.current_time.saturating_add(1),
                            ),
                        )
                    } else {
                        let (completed, result) = clock_periods(sim, *clock_event, batch);
                        (completed, result.map_err(|error| error.to_string()))
                    };
                    if completed == 0 || completed > batch {
                        return ExecResult::Fail(
                            "backend made invalid progress in a reset tick batch".into(),
                        );
                    }
                    ctx.current_time = ctx.current_time.saturating_add(completed);
                    remaining -= completed;
                    if let Err(e) = result {
                        let drained = drain_runtime_assertions(sim, ctx, None);
                        if let Some(message) = drained.fatal_message {
                            return ExecResult::Fail(message_diagnostic(&message));
                        }
                        return ExecResult::Fail(format!("reset: {e}"));
                    }
                    let drained = drain_runtime_assertions(sim, ctx, None);
                    if let Some(message) = drained.fatal_message {
                        return ExecResult::Fail(message_diagnostic(&message));
                    }
                    if drained.finished || sim.components.finish_requested() {
                        return ExecResult::Finished;
                    }
                }
                sim_set_u64(sim, *reset_signal, (*deassert_value).into());
                ExecResult::Continue
            }
            Err(error) => ExecResult::Fail(error.to_string()),
        },
        GenericTestbenchStatement::Assert {
            expr,
            site_id,
            continue_on_fail,
            message,
            location,
        } => {
            let settle = if message.is_some() {
                settle_testbench(sim)
            } else {
                settle_testbench_expr(sim, expr)
            };
            if let Err(e) = settle {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let passed = !eval_expr(sim, expr).is_zero();
            if passed {
                if ctx.record_passes {
                    let rendered_message =
                        render_assert_message(sim, message, ctx.simulation_time());
                    ctx.assertions.push(AssertionResult {
                        passed,
                        message: rendered_message,
                        location: location.clone(),
                    });
                } else {
                    // The message is not rendered, but what its arguments
                    // write still happens.
                    for arg in assert_event_args(message) {
                        if arg.expr.bytecode().writes_state() {
                            eval_expr(sim, &arg.expr);
                        }
                    }
                }
                ExecResult::Continue
            } else {
                publish_tb_assert_event(sim, *site_id, message);
                let rendered_message = drain_runtime_assertions(sim, ctx, location.as_ref())
                    .last_message
                    .or_else(|| render_assert_message(sim, message, ctx.simulation_time()));
                if !continue_on_fail {
                    ExecResult::Fail(message_diagnostic(
                        &rendered_message.unwrap_or_else(|| b"assertion failed".to_vec()),
                    ))
                } else {
                    ExecResult::Continue
                }
            }
        }
        GenericTestbenchStatement::Display { message, newline } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let rendered =
                render_assert_message(sim, message, ctx.simulation_time()).unwrap_or_default();
            forward_display(&rendered, *newline);
            ExecResult::Continue
        }
        GenericTestbenchStatement::If { .. }
        | GenericTestbenchStatement::For { .. }
        | GenericTestbenchStatement::Assign { .. }
        | GenericTestbenchStatement::Break => {
            unreachable!("statement is compiled into the process kernel")
        }
        GenericTestbenchStatement::WriteMemory { signal, load } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            // The file is read now (IEEE 1800-2023 21.4); an error terminates
            // the load, keeping the words loaded before it.
            let outcome = sim.run_testbench_memory_file(load);
            write_testbench_signal(sim, *signal, |sim| {
                sim.apply_testbench_memory_writes(*signal, &outcome.runs)
            });
            sim.report_testbench_memory_file(&outcome, load);
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomSeed { handle, value } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            ctx.random.seed(handle, eval_expr(sim, value).to_u64());
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomGet {
            handle,
            width,
            signed,
            ret,
        } => {
            let value = ctx.random.get(handle, *width);
            if let Some(ret) = ret {
                if ret.selection.is_some()
                    && let Err(e) = settle_testbench(sim)
                {
                    return ExecResult::Fail(format!("eval_comb: {e}"));
                }
                sim_set_random_target(sim, ret, value, *width, *signed);
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomGetRange {
            handle,
            min,
            max,
            width,
            signed,
            ret,
        } => {
            if let Err(e) = settle_testbench(sim) {
                return ExecResult::Fail(format!("eval_comb: {e}"));
            }
            let min = eval_expr(sim, min).to_u64();
            let max = eval_expr(sim, max).to_u64();
            let value = ctx.random.get_range(handle, min, max, *width, *signed);
            if let Some(ret) = ret {
                sim_set_random_target(sim, ret, value, *width, *signed);
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::RandomGetSeed { handle, ret } => {
            let seed = ctx.random.get_seed(handle);
            if let Some(ret) = ret {
                if ret.selection.is_some()
                    && let Err(e) = settle_testbench(sim)
                {
                    return ExecResult::Fail(format!("eval_comb: {e}"));
                }
                sim_set_target(sim, ret, TbValue::U64(seed));
            }
            ExecResult::Continue
        }
        GenericTestbenchStatement::ComponentMethod {
            instance,
            method,
            args,
            ret,
            ret_width,
            ret_signed,
            ret_strict,
        } => {
            if sim.dirty
                && let Err(error) = settle_testbench(sim)
            {
                return ExecResult::Fail(format!("eval_comb: {error}"));
            }
            let host_args = args
                .iter()
                .map(|arg| {
                    crate::component::host_value_from_argument(
                        eval_expr(sim, &arg.expr),
                        arg.width,
                        arg.is_string,
                    )
                })
                .collect::<Vec<_>>();
            let (returned, writes) = match sim.components.call_method(
                instance,
                method,
                &host_args,
                ctx.simulation_time(),
                &mut sim.backend,
            ) {
                Ok(value) => value,
                Err(message) => return ExecResult::Fail(message),
            };
            apply_component_writes(sim, writes);
            if sim.components.finish_requested() {
                return ExecResult::Finished;
            }
            let Some(ret) = ret else {
                return ExecResult::Continue;
            };
            let Some((value, width)) = crate::component::host_bits(&returned) else {
                return ExecResult::Fail(format!(
                    "component method `{instance}.{method}` returned no bit value"
                ));
            };
            if let Some(expected) = ret_width
                && width != *expected
            {
                return ExecResult::Fail(format!(
                    "component method `{method}` declares a {expected}-bit return value but returned {width} bits"
                ));
            }
            if ret_width.is_none() && *ret_strict && width > 64 {
                return ExecResult::Fail(format!(
                    "component method `{method}` returned {width} bits; the expression form carries at most 64 bits"
                ));
            }
            sim_set_target(
                sim,
                ret,
                TbValue::Wide(resize_component_return(
                    value,
                    width as usize,
                    *ret_signed,
                    ret.width,
                )),
            );
            ExecResult::Continue
        }
        GenericTestbenchStatement::Finish => ExecResult::Finished,
    }
}
