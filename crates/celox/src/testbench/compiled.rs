//! Host services for the generated TB scheduler. Clock progress and process
//! dispatch remain in SIR; returning here is a service or a tiering safe point.
use super::TestbenchStatement;
use super::*;
use celox_testbench::*;

// Unit runs, host edge batches and host statement services since the last
// call.
#[cfg(test)]
thread_local! { static COUNTERS: std::cell::Cell<(usize, usize, usize)> = const { std::cell::Cell::new((0, 0, 0)) }; }
#[cfg(test)]
pub(super) fn take_counters() -> (usize, usize, usize) {
    COUNTERS.with(|c| c.replace((0, 0, 0)))
}

pub(super) fn run<B: SimBackend>(
    sim: &mut Simulator<B>,
    tb: &CompiledTestbench<B>,
    ctx: &mut DetailedExecContext,
) -> Option<ExecResult> {
    let m = tb.scheduler()?;
    let entry = m.entry?;
    let semantic = sim.program.testbench.as_ref()?.scheduler()?;
    let clocks: Vec<_> = semantic
        .events
        .iter()
        .map(|a| sim.backend.resolve_event(a))
        .collect();
    let baselines: Vec<_> = semantic
        .baselines
        .iter()
        .zip(&m.baselines)
        // Several addresses can resolve to one event, e.g. a port that
        // copies the clock. Only the event's own signal holds its level:
        // a copy may not be settled when the baseline is saved.
        .filter_map(|((a, _), (_, saved))| {
            let event = sim.backend.resolve_event_opt(a)?;
            (sim.backend.resolve_signal(a) == sim.backend.resolve_signal(&event.addr()))
                .then_some((event.id(), *saved))
        })
        .collect();
    fn collect<'a, B: SimBackend>(
        body: &'a [TestbenchStatement<B>],
        out: &mut Vec<&'a TestbenchStatement<B>>,
    ) {
        for s in body {
            out.push(s);
            match s {
                GenericTestbenchStatement::If {
                    then_block,
                    else_block,
                    ..
                } => {
                    collect::<B>(then_block, out);
                    collect::<B>(else_block, out);
                }
                GenericTestbenchStatement::For { body, .. } => collect::<B>(body, out),
                _ => {}
            }
        }
    }
    let sites: Vec<_> = tb
        .processes()
        .map(|body| {
            let mut v = vec![];
            collect::<B>(body, &mut v);
            v
        })
        .collect();
    // TB assertions report at their source locations.
    let locations: HashMap<usize, SourceLocation> = sites
        .iter()
        .flatten()
        .filter_map(|statement| match statement {
            GenericTestbenchStatement::Assert {
                site_id,
                location: Some(location),
                ..
            } => Some((*site_id as usize, location.clone())),
            _ => None,
        })
        .collect();
    let marker = m.marker_site.map(|site| site as usize);
    let drain = |sim: &mut Simulator<B>, ctx: &mut DetailedExecContext| {
        let Some(marker) = marker else {
            return drain_runtime_assertions(sim, ctx, None);
        };
        let format_ctx = RuntimeFormatContext {
            tb_time: Some(ctx.simulation_time()),
            scope: None,
        };
        let events = sim.drain_marked_runtime_events(format_ctx, marker);
        record_runtime_events(
            ctx,
            events.into_iter().map(|(site, event)| {
                let location = site.and_then(|site| locations.get(&site).cloned());
                (event, location)
            }),
        )
    };
    for &a in &m.control {
        sim.backend.set(a, 0u64);
    }
    sim.backend
        .set(m.control[TB_RECORD], u64::from(ctx.record_passes));
    sim.backend.set(m.control[TB_PHASE], u64::MAX);
    sim.backend
        .set(m.control[TB_LIMIT], ctx.tick_limit.unwrap_or(u64::MAX));
    for p in &m.processes {
        for &a in p {
            sim.backend.set(a, 0u64);
        }
    }
    for (i, c) in m.clocks.iter().enumerate() {
        for &a in c {
            sim.backend.set(a, 0u64);
        }
        sim.backend.set(c[3], u64::from(i < m.declared));
    }
    for k in tb.kernels() {
        sim.backend.set(k.resume, 0u16);
    }
    let dirty = sim
        .backend
        .resolve_signal(&sim.program.runtime_schema.scheduler.as_ref()?.slots.dirty);
    let mut reset_edges = vec![];
    let mut moves = vec![];
    let batch = clock_batch::ClockBatch::new(sim);
    // The last tick whose progress was reported.
    let mut reported = ctx.current_time;
    let mut exits = Exits::default();
    loop {
        let schema = &sim.program.runtime_schema;
        // What keeps edges and settles in the host, named for the log.
        let settle_blockers = [
            ("host_watch", !sim.host_watch.is_empty()),
            ("components", !sim.components.is_empty()),
            ("timed", sim.schedule.timed),
            ("vcd", sim.vcd_writer.is_some()),
            ("comb_observers", !schema.comb_observers.is_empty()),
            ("watches", !schema.watches.is_empty()),
        ];
        let edge_blockers = [
            ("host_slot_work", sim.host_slot_work),
            ("time_slots", !schema.time_slots.is_empty()),
            ("sample_shadows", !schema.sample_shadows.is_empty()),
        ];
        let native_settle = settle_blockers.iter().all(|(_, blocks)| !blocks);
        let native_edges = native_settle && edge_blockers.iter().all(|(_, blocks)| !blocks);
        if exits.blockers.is_empty() {
            exits.blockers = settle_blockers
                .iter()
                .chain(&edge_blockers)
                .filter(|(_, blocks)| *blocks)
                .map(|(name, _)| *name)
                .collect();
        }
        sim.backend
            .set(m.control[TB_NATIVE_EDGES], u64::from(native_edges));
        sim.backend
            .set(m.control[TB_NATIVE_SETTLE], u64::from(native_settle));
        sim.backend.set(dirty, u8::from(sim.dirty));
        // The host drains every event the unit recorded when it returns.
        sim.backend.set(m.control[TB_UNMARKED], 0u64);
        sim.backend.set(m.control[TB_PENDING], 0u64);
        // An interpreted unit returns sooner, so that a tiered backend
        // switches to its compiled code soon after it is ready.
        sim.backend.set(
            m.control[TB_BUDGET],
            if sim.backend.runs_compiled_scheduler() {
                1024u64
            } else {
                64
            },
        );
        #[cfg(test)]
        COUNTERS.with(|c| {
            let (runs, edges, host) = c.get();
            c.set((runs + 1, edges, host));
        });
        if let Err(e) = sim.backend.run_process(entry) {
            // A fatal assertion in a domain the unit ran ends it with an
            // error; its message is in the runtime events.
            let drained = drain(sim, ctx);
            if let Some(message) = drained.fatal_message {
                return Some(ExecResult::Fail(message_diagnostic(&message)));
            }
            return Some(ExecResult::Fail(sim.decorate_runtime_error(e).to_string()));
        }
        sim.dirty = sim.backend.get_as::<u8>(dirty) != 0;
        if sim.dirty {
            sim.backend
                .mark_settle_range(0, sim.backend.layout().total_size);
        }
        sim.backend.clear_triggered_bits();
        let now = sim.backend.get_as::<u64>(m.control[TB_NOW]);
        ctx.scheduler_time = if m.processes.len() == 1 && clocks.len() <= 1 {
            None
        } else {
            Some(sim.backend.get_as(m.control[TB_REPORT_TIME]))
        };
        ctx.current_time = sim.backend.get_as(m.control[TB_TICKS]);
        // Report each progress interval the run crossed, as the host tick
        // batches do.
        if let Some(every) = sim
            .diagnostics
            .testbench_progress_every
            .filter(|every| *every != 0)
        {
            let mut tick = reported - reported % every;
            while let Some(next) = tick.checked_add(every)
                && next <= ctx.current_time
            {
                tracing::debug!("[testbench-progress] tick={next}");
                tick = next;
            }
        }
        reported = ctx.current_time;
        let drained = drain(sim, ctx);
        if let Some(message) = drained.fatal_message {
            return Some(ExecResult::Fail(message_diagnostic(&message)));
        }
        if drained.finished {
            return Some(ExecResult::Finished);
        }
        exits.count(sim.backend.get_as::<u64>(m.control[TB_EXIT]));
        match sim.backend.get_as::<u64>(m.control[TB_EXIT]) {
            TB_DONE => return Some(ExecResult::Continue),
            TB_FINISHED => return Some(ExecResult::Finished),
            TB_LIMITED => {
                ctx.tick_limit_reached = true;
                return Some(ExecResult::Finished);
            }
            TB_OVERFLOW => {
                return Some(ExecResult::Fail(
                    "testbench clock time overflow or invalid scheduler continuation".into(),
                ));
            }
            TB_SAFEPOINT | TB_DRAIN => {}
            TB_FAILED => {
                return Some(ExecResult::Fail(message_diagnostic(
                    &drained
                        .last_message
                        .unwrap_or_else(|| b"assertion failed".to_vec()),
                )));
            }
            TB_HOST | TB_RESET => {
                #[cfg(test)]
                COUNTERS.with(|c| {
                    let (runs, edges, host) = c.get();
                    c.set((runs, edges, host + 1));
                });
                let index = sim.backend.get_as::<u64>(m.control[TB_CURRENT]) as usize;
                let p = m.processes[index];
                let kernel = &tb.kernels()[index];
                let reset = sim.backend.get_as::<u64>(m.control[TB_EXIT]) == TB_RESET;
                let request = if reset {
                    sim.backend.get_as::<u64>(p[4]) as usize - 1
                } else {
                    let encoded = sim.backend.get_as(kernel.request);
                    let Some((celox_design::HostRequestKind::Testbench, index)) =
                        celox_design::HostRequestKind::decode(encoded)
                    else {
                        return Some(ExecResult::Fail("invalid scheduler host request".into()));
                    };
                    index as usize
                };
                match &kernel.requests[request] {
                    KernelRequest::Statement(site) => {
                        let statement = sites[index][*site];
                        if let GenericTestbenchStatement::ClockNext {
                            clock_event, count, ..
                        } = statement
                        {
                            let count = match eval_clock_count(sim, count) {
                                Ok(n) => n,
                                Err(e) => return Some(ExecResult::Fail(e.to_string())),
                            };
                            if count != 0 {
                                let clock = clocks
                                    .iter()
                                    .position(|c| c.id() == clock_event.id())
                                    .unwrap();
                                sim.backend.set(p[0], 1u64);
                                sim.backend.set(p[1], count);
                                sim.backend.set(p[2], clock as u64);
                                if sim.backend.get_as::<u64>(m.clocks[clock][3]) == 0 {
                                    sim.backend.set(m.clocks[clock][3], 1u64);
                                    sim.backend.set(m.clocks[clock][0], now);
                                }
                                sim.backend.set(
                                    m.control[TB_PHASE],
                                    2 + 2 * m.processes.len() as u64 + index as u64,
                                );
                            }
                        } else if let GenericTestbenchStatement::ResetAssert {
                            reset_signal,
                            reset_event,
                            clock_event,
                            duration,
                            assert_value,
                            deassert_value,
                            ..
                        } = statement
                        {
                            if reset {
                                sim_set_u64(sim, *reset_signal, (*deassert_value).into());
                                if let Some(event) = reset_event {
                                    reset_edges.push((*event, *deassert_value));
                                }
                                sim.backend.set(p[4], 0u64);
                            } else {
                                let count = match eval_clock_count(sim, duration) {
                                    Ok(c) => c.max(1),
                                    Err(e) => return Some(ExecResult::Fail(e.to_string())),
                                };
                                sim_set_u64(sim, *reset_signal, (*assert_value).into());
                                if let Some(event) = reset_event {
                                    reset_edges.push((*event, *assert_value));
                                }
                                let clock = clocks
                                    .iter()
                                    .position(|c| c.id() == clock_event.id())
                                    .unwrap();
                                sim.backend.set(p[0], 1u64);
                                sim.backend.set(p[1], count);
                                sim.backend.set(p[2], clock as u64);
                                sim.backend.set(p[4], request as u64 + 1);
                                if sim.backend.get_as::<u64>(m.clocks[clock][3]) == 0 {
                                    sim.backend.set(m.clocks[clock][3], 1u64);
                                    sim.backend.set(m.clocks[clock][0], now);
                                }
                                sim.backend.set(
                                    m.control[TB_PHASE],
                                    2 + 2 * m.processes.len() as u64 + index as u64,
                                );
                            }
                        } else {
                            let result = exec_one_detailed(sim, statement, ctx);
                            if matches!(result, ExecResult::Finished) {
                                if m.processes.len() == 1 && clocks.len() <= 1 {
                                    return Some(result);
                                }
                                sim.backend.set(m.control[TB_STOPPED], 1u64);
                                sim.backend.set(p[0], 2u64);
                                sim.backend.set(
                                    m.control[TB_PHASE],
                                    2 + 2 * m.processes.len() as u64 + index as u64,
                                );
                            } else if !matches!(result, ExecResult::Continue) {
                                return Some(result);
                            }
                        }
                    }
                    KernelRequest::Settle { .. } => {
                        if let Err(e) = settle_testbench(sim) {
                            return Some(ExecResult::Fail(e.to_string()));
                        }
                    }
                    KernelRequest::Error(message) => {
                        return Some(ExecResult::Fail(message.clone()));
                    }
                    _ => return Some(ExecResult::Fail("unexpected scheduler host service".into())),
                }
            }
            TB_EDGES => {
                #[cfg(test)]
                COUNTERS.with(|c| {
                    let (runs, edges, host) = c.get();
                    c.set((runs, edges + 1, host));
                });
                let mut state = sim
                    .component_simulation
                    .take()
                    .unwrap_or_else(|| Box::new(crate::simulation::simulation_state(sim)));
                let levels = baselines
                    .iter()
                    .map(|&(id, saved)| {
                        let (value, mask) = sim.backend.get_four_state(saved);
                        let value = u8::from(value != BigUint::from(0u8));
                        let unknown = u8::from(mask != BigUint::from(0u8))
                            * celox_design::edges::LEVEL_UNKNOWN;
                        (id, value | unknown)
                    })
                    .collect::<Vec<_>>();
                state.restore_event_baselines(levels);
                sim.component_simulation = Some(state);
                let mut events = vec![];
                let mut rising = false;
                for (clock, c) in clocks.iter().zip(&m.clocks) {
                    if sim.backend.get_as::<u64>(c[2]) != 0
                        && sim.backend.get_as::<u64>(c[1]) == now
                    {
                        events.push((*clock, 0));
                    }
                }
                for (clock, c) in clocks.iter().zip(&m.clocks) {
                    if !ctx.at_tick_limit()
                        && sim.backend.get_as::<u64>(m.control[TB_STOPPED]) == 0
                        && sim.backend.get_as::<u64>(c[3]) != 0
                        && sim.backend.get_as::<u64>(c[0]) == now
                    {
                        events.push((*clock, 1));
                        rising = true;
                    }
                }
                events.append(&mut reset_edges);
                let resets = m
                    .processes
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| {
                        let request = sim.backend.get_as::<u64>(p[4]);
                        if request == 0 {
                            return None;
                        }
                        let KernelRequest::Statement(site) =
                            tb.kernels()[i].requests[request as usize - 1]
                        else {
                            return None;
                        };
                        let GenericTestbenchStatement::ResetAssert {
                            clock_event,
                            reset_event,
                            ..
                        } = sites[i][site]
                        else {
                            return None;
                        };
                        Some((*clock_event, *reset_event))
                    })
                    .collect::<Vec<_>>();
                let result = step_process_events(
                    sim,
                    &events,
                    &resets,
                    now,
                    rising,
                    ctx,
                    &mut moves,
                    batch.as_ref(),
                );
                if !matches!(result, ExecResult::Continue) {
                    return Some(result);
                }
            }
            _ => {
                return Some(ExecResult::Fail(
                    "invalid compiled TB scheduler exit".into(),
                ));
            }
        }
        // Saved function temporaries are layout-owned roots throughout a run.
        sim.maybe_collect_heap([]);
    }
}

/// Returns of the generated TB scheduler to the host, logged when a run ends:
/// each one is a service or safe point the generated code could not provide.
#[derive(Default)]
struct Exits {
    host: u64,
    reset: u64,
    edges: u64,
    safepoint: u64,
    drain: u64,
    /// Design features that keep settles or edges in the host.
    blockers: Vec<&'static str>,
}

impl Exits {
    fn count(&mut self, exit: u64) {
        match exit {
            TB_HOST => self.host += 1,
            TB_RESET => self.reset += 1,
            TB_EDGES => self.edges += 1,
            TB_SAFEPOINT => self.safepoint += 1,
            TB_DRAIN => self.drain += 1,
            _ => {}
        }
    }
}

impl Drop for Exits {
    fn drop(&mut self) {
        tracing::debug!(
            host = self.host,
            reset = self.reset,
            edges = self.edges,
            safepoint = self.safepoint,
            drain = self.drain,
            blockers = ?self.blockers,
            "[testbench-exits]"
        );
    }
}
