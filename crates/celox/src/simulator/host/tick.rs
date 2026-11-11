//! Clock ticks, explicit edges and derived clock domains.

use super::*;

/// Accumulated timings of ticks, reported every `tick_timing_every` ticks.
#[derive(Default)]
pub(super) struct TickTiming {
    pub(super) ticks: u64,
    pub(super) eval_apply_ns: u64,
    pub(super) eval_comb_ns: u64,
}

/// The derived clock domains of a design and what running them needs.
pub(super) struct DerivedClocks<B: SimBackend> {
    /// Per event ID, the derived clock domains a tick can trigger.
    /// Empty for a design without derived clocks.
    pub(super) ticks: celox_runtime::DerivedTickPlans<B::Event>,
    /// Boxed, so that taking it out for a tick moves one pointer.
    pub(super) scratch: Option<Box<celox_runtime::DerivedTickScratch>>,
    pub(super) coincident_edges: celox_runtime::CoincidentEdgeCache<B::Event>,
    /// Per event ID, the generated units that run an edge of the event
    /// with its derived domains. Empty without them.
    pub(super) units: Arc<[Option<DerivedEdgeUnit>]>,
    /// The flag a derived-edge unit reads to settle combinational logic
    /// before the domains evaluate.
    pub(super) settle: Option<SignalRef>,
}

impl<B: SimBackend> DerivedClocks<B> {
    pub(super) fn new(
        backend: &B,
        schedule: &celox_design::schedule::ScheduleModel<crate::AbsoluteAddr>,
        schema: &celox_design::RuntimeSchema<crate::ir::AbsoluteAddr>,
    ) -> Self {
        let (ticks, scratch) = celox_runtime::build_derived_tick_plans(backend, schedule);
        let (units, settle) = derived_edge_units(backend, schema, &ticks);
        Self {
            ticks,
            scratch: Some(Box::new(scratch)),
            coincident_edges: Default::default(),
            units,
            settle,
        }
    }
}

/// The generated units of one event's edges with its derived domains.
#[derive(Clone, Copy, Debug)]
pub(super) struct DerivedEdgeUnit {
    pub(super) rising: usize,
    pub(super) falling: usize,
    /// The event gates derived clocks, so an edge always runs domains.
    pub(super) gated: bool,
}

/// Per event ID, the generated derived-edge units of `schema`, and the flag
/// they read to settle first.
pub(super) fn derived_edge_units<B: SimBackend>(
    backend: &B,
    schema: &celox_design::RuntimeSchema<crate::ir::AbsoluteAddr>,
    plans: &celox_runtime::DerivedTickPlans<B::Event>,
) -> (Arc<[Option<DerivedEdgeUnit>]>, Option<SignalRef>) {
    if schema.derived_edges.is_empty() {
        return (Arc::from([]), None);
    }
    let mut units = vec![None; backend.num_events()];
    for edges in &schema.derived_edges {
        let Some(event) = backend.resolve_event_opt(&edges.event) else {
            continue;
        };
        let Some(Some(plan)) = plans.get(event.id()) else {
            continue;
        };
        units[event.id()] = Some(DerivedEdgeUnit {
            rising: edges.rising,
            falling: edges.falling,
            gated: plan.lever_signals().next().is_some(),
        });
    }
    let flag = schema
        .scheduler
        .as_ref()
        .map(|scheduler| backend.resolve_signal(&scheduler.slots.dirty));
    (units.into(), flag)
}

impl<B: SimBackend> Simulator<B> {
    pub(super) fn record_tick_timing(&mut self, eval_apply_ns: u64, eval_comb_ns: u64) {
        let Some(every) = self.diagnostics.tick_timing_every else {
            return;
        };
        if every == 0 {
            return;
        }
        self.tick_timing.ticks = self.tick_timing.ticks.saturating_add(1);
        self.tick_timing.eval_apply_ns =
            self.tick_timing.eval_apply_ns.saturating_add(eval_apply_ns);
        self.tick_timing.eval_comb_ns = self.tick_timing.eval_comb_ns.saturating_add(eval_comb_ns);
        if self.tick_timing.ticks.is_multiple_of(every) {
            tracing::debug!(
                "[tick-timing] ticks={} eval_apply_ms={:.3} eval_comb_ms={:.3} avg_apply_us={:.3} avg_comb_us={:.3}",
                self.tick_timing.ticks,
                self.tick_timing.eval_apply_ns as f64 / 1_000_000.0,
                self.tick_timing.eval_comb_ns as f64 / 1_000_000.0,
                self.tick_timing.eval_apply_ns as f64 / self.tick_timing.ticks as f64 / 1_000.0,
                self.tick_timing.eval_comb_ns as f64 / self.tick_timing.ticks as f64 / 1_000.0,
            );
        }
    }

    /// One clock period of an event's signal: its active edge, then its
    /// inactive edge.
    ///
    /// A tick is defined as the SystemVerilog statements
    ///
    /// ```systemverilog
    /// s <= A; #H;
    /// s <= I; #L;
    /// ```
    ///
    /// on the event's signal `s`, where `A` is its active level (1 for a
    /// positive-edge clock or an active-high reset, 0 otherwise) and `I`
    /// the inactive one, with host operations taking place between
    /// periods. Every domain sensitive to either edge runs, including
    /// domains of the opposite edge and clocks derived from the signal,
    /// and the signal ends at the inactive level. A signal already at the
    /// active level takes no active edge. See the execution semantics
    /// guide for the host API as a whole.
    pub fn tick(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        if self.frozen {
            return Ok(());
        }
        let info = self.event_edges[event.id()];
        let active = info.active_edge();
        let inactive = active.opposite();
        if info.inactive_runs || !self.host_watch.is_empty() {
            self.active_edge(event)?;
            return self.inactive_edge(event);
        }
        if let Some(Some(_)) = self.derived.ticks.get(event.id()) {
            return self.tick_derived_quiet(event);
        }
        // Everything is read before the domain runs: right after its
        // kernel, loads of host tables can wait behind the kernel's
        // stores (4 KiB aliasing), which made a period cost almost two
        // domain runs. The inactive edge runs nothing and only stores.
        let observers = !self.program.runtime_schema.comb_observers.is_empty();
        if !info.level_read && !observers && celox_runtime::at_level(&self.backend, &info, inactive)
        {
            // Nothing the domain runs reads the level, which is the
            // inactive one before and after the period: both stores are
            // unobservable, and the period is one run of the domain.
            // The call begins its slot in the domain's run.
            return self.fire_from::<true>(event);
        }
        self.begin_host_slot()?;
        let moved =
            celox_runtime::move_signal(&mut self.backend, &info, active, celox_runtime::set_level);
        if let Some(old) = moved {
            if self.clock_move_needs_settle(event, info.comb_read)? {
                self.dirty = true;
            }
            if info.fires(old, active) {
                self.fire(event)?;
            }
        }
        self.backend.set(info.signal, inactive.level());
        if self.clock_move_needs_settle(event, info.comb_read)? || observers {
            self.dirty = true;
        }
        Ok(())
    }

    /// [`Self::tick`] of an event with derived domains whose inactive
    /// edge runs nothing: the active edge, then only the inactive level.
    #[inline(never)]
    pub(super) fn tick_derived_quiet(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.active_edge(event)?;
        let info = self.event_edges[event.id()];
        if self.dirty {
            // Combinational logic did not settle, so the inactive edge
            // may still find a derived clock moved by the settle.
            return self.inactive_edge(event);
        }
        self.backend
            .set(info.signal, info.active_edge().opposite().level());
        if self.clock_move_needs_settle(event, info.comb_read)?
            || !self.program.runtime_schema.comb_observers.is_empty()
        {
            self.dirty = true;
        }
        Ok(())
    }

    /// Publish wiring immediately when the previous state was settled;
    /// otherwise leave the entire pending evaluation to the normal path.
    pub(super) fn clock_move_needs_settle(
        &mut self,
        event: B::Event,
        comb_read: bool,
    ) -> Result<bool, RuntimeErrorCode> {
        Ok(comb_read && (self.dirty || !self.backend.eval_clock_aliases(event)?))
    }

    /// Run the event's domain as its active edge does, with the signal
    /// left as it is: the unit of the edge APIs.
    pub(super) fn fire(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.fire_from::<false>(event)
    }

    /// [`Self::fire`]; with `BEGIN`, also the start of the host call,
    /// whose slot [`Self::begin_host_slot`] begins in the domain's run,
    /// where the clock that nothing reads has already moved.
    #[inline]
    pub(super) fn fire_from<const BEGIN: bool>(
        &mut self,
        event: B::Event,
    ) -> Result<(), RuntimeErrorCode> {
        let result = self.fire_unit::<BEGIN>(event);
        self.heap_safepoint();
        result.and(self.take_heap_fault())?;
        self.end_host_slot()
    }

    pub(super) fn fire_unit<const BEGIN: bool>(
        &mut self,
        event: B::Event,
    ) -> Result<(), RuntimeErrorCode> {
        if let Some(Some(_)) = self.derived.ticks.get(event.id()) {
            return self.tick_derived(event);
        }
        let timing_enabled = self.diagnostics.tick_timing_every.is_some();
        let mut eval_comb_ns = 0u64;
        let mut eval_apply_ns = 0u64;
        if self.dirty {
            if BEGIN && self.slot_end.work && !self.timed {
                self.settle_host_slot()?;
                self.eval_apply_ff_at_checked(event)?;
            } else if timing_enabled {
                let start = crate::timing::now();
                self.eval_comb_checked()?;
                eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
                let start = crate::timing::now();
                self.eval_apply_ff_at_checked(event)?;
                eval_apply_ns = eval_apply_ns.saturating_add(start.elapsed().as_nanos() as u64);
            } else {
                self.eval_comb_checked()?;
                self.eval_apply_ff_at_checked(event)?;
            }
            self.dirty = false;
        } else {
            if timing_enabled {
                let start = crate::timing::now();
                self.eval_apply_ff_at_checked(event)?;
                eval_apply_ns = eval_apply_ns.saturating_add(start.elapsed().as_nanos() as u64);
            } else {
                self.eval_apply_ff_at_checked(event)?;
            }
        }
        if timing_enabled {
            let start = crate::timing::now();
            self.eval_comb_checked()?;
            eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
            self.record_tick_timing(eval_apply_ns, eval_comb_ns);
        } else {
            self.eval_comb_checked()?;
        }
        self.dirty = false;
        Ok(())
    }

    /// Move `event`'s signal through `edge` and run every domain
    /// sensitive to that transition, as a timed step does: the event's own
    /// domain when `edge` is its active edge, clocks computed
    /// combinationally from the signal that see their active edge (judged
    /// from the state before the edge, evaluating with the event's own
    /// domain), and, after the commit, gates the commit moves and domains
    /// clocked from state, transitively. The signal stays at its new
    /// level. An edge to the level the signal already has does nothing.
    ///
    /// An unknown (X) level counts as either level, as in SystemVerilog.
    pub fn edge(
        &mut self,
        event: B::Event,
        edge: celox_runtime::Edge,
    ) -> Result<(), RuntimeErrorCode> {
        // A timed simulation the design finished no longer changes.
        if self.frozen {
            return Ok(());
        }
        let info = self.event_edges[event.id()];
        if !matches!(self.derived.ticks.get(event.id()), Some(Some(_))) {
            if !info.can_fire(edge) && self.host_watch.is_empty() {
                // An inactive edge runs nothing, whatever the level was:
                // store the level without reading it first. A load of
                // the signal right after the previous edge's kernel can
                // wait behind the kernel's stores (4 KiB aliasing),
                // which made a period cost almost two domain runs.
                self.backend.set(info.signal, edge.level());
                if self.clock_move_needs_settle(event, info.comb_read)?
                    || !self.program.runtime_schema.comb_observers.is_empty()
                {
                    self.dirty = true;
                }
                return Ok(());
            }
            // No derived domains: move the signal, then run the event's
            // domain. Logic that reads the signal settles before it
            // moves; otherwise the call begins its slot in the domain's
            // run.
            if info.comb_read {
                self.begin_host_slot()?;
            }
            let write = watched_write(&mut self.host_watch);
            let Some(old) = celox_runtime::move_signal(&mut self.backend, &info, edge, write)
            else {
                return Ok(());
            };
            if self.clock_move_needs_settle(event, info.comb_read)? {
                self.dirty = true;
            }
            if info.fires(old, edge) {
                return if info.comb_read {
                    self.fire(event)
                } else {
                    self.fire_from::<true>(event)
                };
            }
            if !self.program.runtime_schema.comb_observers.is_empty() {
                self.dirty = true;
            }
            return Ok(());
        }
        self.begin_host_slot()?;
        if let Some(Some(unit)) = self.derived.units.get(event.id()).copied()
            && !self.finish_sites
        {
            self.backend.request_derived_edge_units();
            if self.backend.runs_derived_edge_units() {
                return self.edge_unit(event, info, edge, unit);
            }
        }
        let plans = std::sync::Arc::clone(&self.derived.ticks);
        let plan = plans.get(event.id()).and_then(Option::as_ref);
        let mut scratch = self.derived.scratch.take().unwrap_or_default();
        let mut watch = std::mem::take(&mut self.host_watch);
        let dirty = self.dirty;
        let write = watched_write(&mut watch);
        let result =
            celox_runtime::run_edge(self, plan, info, &mut scratch, event, edge, dirty, write);
        self.host_watch = watch;
        self.derived.scratch = Some(scratch);
        self.note_edge(result)
    }

    /// [`Self::edge`] of an event with a generated derived-edge unit:
    /// move the signal, then run the unit of the edge, which judges the
    /// gated clocks, runs the domains and the cascade rounds, as the plan
    /// walk does.
    #[inline(never)]
    pub(super) fn edge_unit(
        &mut self,
        event: B::Event,
        info: celox_runtime::EventEdge,
        edge: celox_runtime::Edge,
        unit: DerivedEdgeUnit,
    ) -> Result<(), RuntimeErrorCode> {
        let mut watch = std::mem::take(&mut self.host_watch);
        let moved =
            celox_runtime::move_signal(&mut self.backend, &info, edge, watched_write(&mut watch));
        self.host_watch = watch;
        let Some(old) = moved else {
            return Ok(());
        };
        let fires = info.fires(old, edge);
        let comb_read = self.clock_move_needs_settle(event, info.comb_read)?;
        if let Some(flag) = self.derived.settle {
            self.backend.set(flag, u8::from(self.dirty || comb_read));
        }
        let index = match edge {
            celox_runtime::Edge::Rising => unit.rising,
            celox_runtime::Edge::Falling => unit.falling,
        };
        if let Err(error) = self.backend.run_process(index) {
            self.dirty = true;
            return Err(self.decorate_runtime_error(error));
        }
        // Without a gated clock and the event's own edge nothing ran, as
        // in the plan walk.
        let outcome = if unit.gated || fires {
            celox_runtime::EdgeOutcome::Settled
        } else if comb_read {
            celox_runtime::EdgeOutcome::Moved
        } else {
            celox_runtime::EdgeOutcome::Stored
        };
        self.note_edge(Ok(outcome))
    }

    /// Settle the outcome of [`celox_runtime::run_edge`] or
    /// [`celox_runtime::run_edges`], and end the time slot they ran.
    pub(super) fn note_edge(
        &mut self,
        outcome: Result<celox_runtime::EdgeOutcome, celox_runtime::Halt>,
    ) -> Result<(), RuntimeErrorCode> {
        match outcome {
            Ok(celox_runtime::EdgeOutcome::Unchanged) => {}
            Ok(celox_runtime::EdgeOutcome::Moved) => self.dirty = true,
            // Combinational logic does not read the signal; observers
            // still see its new level at their next evaluation.
            Ok(celox_runtime::EdgeOutcome::Stored) => {
                if !self.program.runtime_schema.comb_observers.is_empty() {
                    self.dirty = true;
                }
            }
            Ok(celox_runtime::EdgeOutcome::Settled) => self.dirty = false,
            // An RTL `$finish` or `$stop` ends or suspends only a timed
            // simulation; an edge, as a tick, leaves it to the host. What
            // ran may not have settled.
            Err(celox_runtime::Halt::Finish | celox_runtime::Halt::Stop) => self.dirty = true,
            Err(celox_runtime::Halt::Error(error)) => return Err(error),
        }
        // The edges are a time slot, as a tick is.
        self.end_host_slot()
    }

    /// A rising edge of `event`'s signal; see [`Self::edge`].
    pub fn posedge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.edge(event, celox_runtime::Edge::Rising)
    }

    /// A falling edge of `event`'s signal; see [`Self::edge`].
    pub fn negedge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        self.edge(event, celox_runtime::Edge::Falling)
    }

    /// The event's active edge (rising for a positive-edge clock or an
    /// active-high reset, falling otherwise); see [`Self::edge`].
    pub fn active_edge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        let edge = self.event_edges[event.id()].active_edge();
        self.edge(event, edge)
    }

    /// The edge opposite to the event's active edge; see [`Self::edge`].
    pub fn inactive_edge(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        let edge = match self.event_edges[event.id()].active_edge() {
            celox_runtime::Edge::Rising => celox_runtime::Edge::Falling,
            celox_runtime::Edge::Falling => celox_runtime::Edge::Rising,
        };
        self.edge(event, edge)
    }

    /// Move several event signals at one instant, as coincident edges:
    /// every domain sensitive to one of them evaluates against the state
    /// before the edges, then all of them commit, then the rounds of
    /// [`Self::edge`] follow.
    pub fn edges(
        &mut self,
        moves: &[(B::Event, celox_runtime::Edge)],
    ) -> Result<(), RuntimeErrorCode> {
        if self.frozen {
            return Ok(());
        }
        self.begin_host_slot()?;
        let plans = std::sync::Arc::clone(&self.derived.ticks);
        let edges = std::sync::Arc::clone(&self.event_edges);
        let mut scratch = self.derived.scratch.take().unwrap_or_default();
        let mut watch = std::mem::take(&mut self.host_watch);
        let dirty = self.dirty;
        let write = watched_write(&mut watch);
        let result = if moves.len() < 2 {
            celox_runtime::run_edges(self, &plans, &edges, &mut scratch, moves, dirty, write)
        } else {
            let mut cache = std::mem::take(&mut self.derived.coincident_edges);
            let result = cache.run(self, &plans, &edges, &mut scratch, moves, dirty, write);
            self.derived.coincident_edges = cache;
            result
        };
        self.host_watch = watch;
        self.derived.scratch = Some(scratch);
        self.note_edge(result)
    }

    /// Per event ID, its signal and polarity, for hosts that drive the
    /// backend without this facade.
    pub fn event_edges(&self) -> celox_runtime::EventEdges {
        std::sync::Arc::clone(&self.event_edges)
    }

    /// Per event ID, the derived clock domains a tick of the event can
    /// trigger, for hosts that drive the backend without this facade.
    /// Empty when the design has no derived clocks.
    pub fn derived_tick_plans(&self) -> celox_runtime::DerivedTickPlans<B::Event> {
        std::sync::Arc::clone(&self.derived.ticks)
    }

    /// The domain run of an event whose edges can trigger derived
    /// domains, kept out of line so that the plain run stays small.
    #[inline(never)]
    pub(super) fn tick_derived(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        let plans = std::sync::Arc::clone(&self.derived.ticks);
        let Some(Some(plan)) = plans.get(event.id()) else {
            unreachable!("tick_derived needs a plan");
        };
        let mut scratch = self.derived.scratch.take().unwrap_or_default();
        let dirty = self.dirty;
        let result = plan.tick(self, &mut scratch, dirty);
        self.derived.scratch = Some(scratch);
        if result.is_ok() {
            self.dirty = false;
        }
        result
    }

    /// [`Self::tick_events`] of coincident domains, one of which can
    /// trigger derived domains: the ticked domains evaluate and commit
    /// together, then the derived domains of all of them follow.
    #[cold]
    pub(super) fn tick_events_derived(
        &mut self,
        domains: &[(B::Event, B::Event, B::Event)],
        unsplit: &[B::Event],
    ) -> Result<(), RuntimeErrorCode> {
        let sources: Vec<_> = domains
            .iter()
            .map(|&(event, eval_only, apply)| celox_runtime::EventDomain {
                eval_apply: event,
                eval_only: Some(eval_only),
                apply: Some(apply),
            })
            .chain(unsplit.iter().map(|&event| celox_runtime::EventDomain {
                eval_apply: event,
                eval_only: None,
                apply: None,
            }))
            .collect();
        let plans = std::sync::Arc::clone(&self.derived.ticks);
        let plan = celox_runtime::DerivedTickPlan::merge(
            sources,
            domains
                .iter()
                .map(|(event, _, _)| event)
                .chain(unsplit)
                .filter_map(|event| plans.get(event.id())?.as_ref()),
        );
        let mut scratch = self.derived.scratch.take().unwrap_or_default();
        let dirty = self.dirty;
        let result = plan.tick(self, &mut scratch, dirty);
        self.derived.scratch = Some(scratch);
        if result.is_ok() {
            self.dirty = false;
        }
        result
    }

    /// Tick several events at once: [`Self::tick`] with the edges of all
    /// their signals made together,
    ///
    /// ```systemverilog
    /// s1 <= A1; ... sk <= Ak; #H;
    /// s1 <= I1; ... sk <= Ik; #L;
    /// ```
    ///
    /// so every domain triggered by the coincident active edges samples
    /// the state settled before them, then all of them update. This is
    /// the behaviour of clocks that share an edge in hardware, such as a
    /// divided clock and its source, which sequential [`Self::tick`]
    /// calls cannot express: each of them would settle the design
    /// between the domains. Events of the same domain are applied once.
    /// A single event behaves like [`Self::tick`].
    pub fn tick_events(&mut self, events: &[B::Event]) -> Result<(), RuntimeErrorCode> {
        if self.frozen {
            return Ok(());
        }
        self.begin_host_slot()?;
        if let [event] = events {
            return self.tick(*event);
        }
        let only_active_edges = self.host_watch.is_empty()
            && events
                .iter()
                .all(|event| !self.event_edges[event.id()].inactive_runs);
        let derived = events
            .iter()
            .any(|event| matches!(self.derived.ticks.get(event.id()), Some(Some(_))));
        if !only_active_edges || derived {
            let edges = std::sync::Arc::clone(&self.event_edges);
            let moves = |edge: fn(celox_runtime::EventEdge) -> celox_runtime::Edge| {
                events
                    .iter()
                    .map(|&event| (event, edge(edges[event.id()])))
                    .collect::<Vec<_>>()
            };
            let active = moves(|info| info.active_edge());
            self.edges(&active)?;
            if !only_active_edges || self.dirty {
                let inactive = moves(|info| info.active_edge().opposite());
                return self.edges(&inactive);
            }
            // The inactive edges run nothing and only store.
            for &event in events {
                let info = self.event_edges[event.id()];
                self.backend
                    .set(info.signal, info.active_edge().opposite().level());
                if info.comb_read {
                    self.dirty = true;
                }
            }
            if !self.program.runtime_schema.comb_observers.is_empty() {
                self.dirty = true;
            }
            return Ok(());
        }
        // The inactive edges run nothing and only store.
        let mut fired = Vec::with_capacity(events.len());
        for &event in events {
            let info = self.event_edges[event.id()];
            let active = info.active_edge();
            if let Some(old) = celox_runtime::move_signal(
                &mut self.backend,
                &info,
                active,
                celox_runtime::set_level,
            ) {
                if info.comb_read {
                    self.dirty = true;
                }
                if info.fires(old, active) {
                    fired.push(event);
                }
            }
        }
        let result = self.tick_events_unit(&fired);
        self.heap_safepoint();
        result.and(self.take_heap_fault())?;
        self.end_host_slot()?;
        for &event in events {
            let info = self.event_edges[event.id()];
            self.backend
                .set(info.signal, info.active_edge().opposite().level());
            if info.comb_read {
                self.dirty = true;
            }
        }
        if !self.program.runtime_schema.comb_observers.is_empty() {
            self.dirty = true;
        }
        Ok(())
    }

    pub(super) fn tick_events_unit(&mut self, events: &[B::Event]) -> Result<(), RuntimeErrorCode> {
        use celox_runtime::backend::EventHandle;
        // Deduplicate domains before requiring split kernels: a design
        // with a single domain has no split kernels, and aliases of one
        // domain are an ordinary tick.
        let mut unique: Vec<(crate::ir::AbsoluteAddr, B::Event)> = Vec::with_capacity(events.len());
        for event in events {
            let addr = self.backend.id_to_addr_slice()[event.id()];
            let canonical = self.schedule.events.canonical(addr);
            if unique.iter().all(|(existing, _)| *existing != canonical) {
                unique.push((canonical, *event));
            }
        }
        // Split kernels exist for every active domain once a design has
        // more than one. A domain without them drives no FF, unless the
        // design has at most one active domain and no split kernels at
        // all; then ticking each event in turn is exact, because at most
        // one of them changes state.
        let mut domains = Vec::with_capacity(unique.len());
        let mut unsplit = Vec::new();
        for (canonical, event) in unique {
            match celox_runtime::EventDomain::split_kernels(&self.backend, &canonical) {
                (Some(eval_only), Some(apply)) => domains.push((event, eval_only, apply)),
                _ => unsplit.push(event),
            }
        }
        if !self.derived.ticks.is_empty()
            && domains.len() + unsplit.len() > 1
            && domains
                .iter()
                .map(|(event, _, _)| event)
                .chain(&unsplit)
                .any(|event| matches!(self.derived.ticks.get(event.id()), Some(Some(_))))
        {
            return self.tick_events_derived(&domains, &unsplit);
        }
        match domains.as_slice() {
            [] => {
                for event in unsplit {
                    self.fire(event)?;
                }
                return Ok(());
            }
            [(event, _, _)] => return self.fire(*event),
            _ => {}
        }
        let timing_enabled = self.diagnostics.tick_timing_every.is_some();
        let mut eval_comb_ns = 0u64;
        let mut eval_apply_ns = 0u64;
        if self.dirty {
            let start = timing_enabled.then(crate::timing::now);
            self.eval_comb_checked()?;
            if let Some(start) = start {
                eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
            }
            self.dirty = false;
        }
        let start = timing_enabled.then(crate::timing::now);
        for (_, eval_only, _) in &domains {
            self.eval_only_ff_at_checked(*eval_only)?;
        }
        for (_, _, apply) in &domains {
            self.apply_ff_at_checked(*apply)?;
        }
        if let Some(start) = start {
            eval_apply_ns = eval_apply_ns.saturating_add(start.elapsed().as_nanos() as u64);
        }
        let start = timing_enabled.then(crate::timing::now);
        self.eval_comb_checked()?;
        if let Some(start) = start {
            eval_comb_ns = eval_comb_ns.saturating_add(start.elapsed().as_nanos() as u64);
            self.record_tick_timing(eval_apply_ns, eval_comb_ns);
        }
        self.dirty = false;
        Ok(())
    }

    /// Advance one event while deferring the post-edge combinational settle.
    /// Native testbenches use this between observable expressions so the
    /// preceding comb phase and this FF phase can share one compiled function.
    pub(crate) fn tick_deferred_comb(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        if !self.program.runtime_schema.comb_observers.is_empty()
            || matches!(self.derived.ticks.get(event.id()), Some(Some(_)))
            || self.slot_end.work
        {
            // Derived domains need the settled state to see their edges,
            // and each tick of a design with slot work is a host call.
            self.begin_host_slot()?;
            return self.fire(event);
        }
        if self.dirty {
            self.eval_comb_apply_ff_at_checked(event)?;
        } else {
            self.eval_apply_ff_at_checked(event)?;
        }
        self.dirty = true;
        Ok(())
    }

    /// The edge facts of `event`.
    #[cfg(all(test, feature = "systemverilog", feature = "cranelift"))]
    pub(crate) fn event_edge(&self, event: B::Event) -> celox_runtime::EventEdge {
        self.event_edges[event.id()]
    }

    /// Whether a period of `event`'s signal, a rise and a fall, runs
    /// domains only at its active edge: the event's own and those derived
    /// from it, while the inactive edge runs nothing.
    pub(crate) fn period_runs_only_active_edge(&self, event: B::Event) -> bool {
        !self.event_edges[event.id()].inactive_runs && self.host_watch.is_empty()
    }

    /// Advance a run of identical deferred-comb ticks. Native code may keep
    /// the loop inside the generated function, but must return after publishing
    /// a runtime event so host-side observation remains tick-accurate.
    pub(crate) fn tick_deferred_comb_many(
        &mut self,
        event: B::Event,
        count: u64,
    ) -> (u64, Result<(), RuntimeErrorCode>) {
        if count == 0 {
            return (0, Ok(()));
        }
        if !self.program.runtime_schema.comb_observers.is_empty()
            || !self.dirty
            || matches!(self.derived.ticks.get(event.id()), Some(Some(_)))
            || self.slot_end.work
        {
            return (1, self.tick_deferred_comb(event));
        }
        let (completed, result) = self.backend.eval_comb_apply_ff_many_at(event, count);
        self.dirty = true;
        (
            completed,
            result.map_err(|error| self.decorate_runtime_error(error)),
        )
    }

    /// Triggers a clock/event by its numeric ID.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeErrorCode::NotAnEvent`] if `event_id` is not a
    /// known event ID, or a runtime error if evaluating the event fails.
    pub fn tick_by_id(&mut self, event_id: usize) -> Result<(), RuntimeErrorCode> {
        let event = self
            .backend
            .id_to_event_slice()
            .get(event_id)
            .copied()
            .ok_or_else(|| RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))?;
        self.tick(event)
    }

    /// Triggers a clock/event N times by its numeric ID.
    /// Avoids repeated cross-boundary calls when used from FFI.
    /// The event ID is validated even when `count` is zero.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeErrorCode::NotAnEvent`] if `event_id` is not a
    /// known event ID, or a runtime error if evaluating an event fails.
    pub fn tick_by_id_n(&mut self, event_id: usize, count: u32) -> Result<(), RuntimeErrorCode> {
        let event = self
            .backend
            .id_to_event_slice()
            .get(event_id)
            .copied()
            .ok_or_else(|| RuntimeErrorCode::NotAnEvent(format!("event_id={event_id}")))?;
        for _ in 0..count {
            self.tick(event)?;
        }
        Ok(())
    }
}
