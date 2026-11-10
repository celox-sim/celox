//! Generating the scheduler unit of a [`Plan`]: the loop over the time slots.
mod kernels;
mod slot_end;
mod slot_start;

use super::*;

/// What the phases of the slot loop share: the kernels and events of the
/// plan as the unit numbers them, and the blocks its regions meet at.
struct Slot<'a> {
    program: &'a LaidOutProgram,
    slots: &'a SchedulerSlots<AbsoluteAddr>,
    schema: &'a celox_design::RuntimeSchema<AbsoluteAddr>,
    /// See [`Plan::edged`].
    edged: &'a [usize],
    /// See [`Plan::tracked`].
    tracked: &'a [usize],
    /// The width of a mask of the host clocks.
    clock_bits: usize,
    /// The kernels that wait for edges, with their waits.
    waits: &'a [(usize, &'a Wait)],
    /// The kernels that delay, with the status slot of those that also wait.
    delays: &'a [(usize, Option<AbsoluteAddr>)],
    /// The bit of each kernel in the ready mask.
    ordinals: &'a [usize],
    checks: &'a [WaitCheck<'a>],
    /// The kernels that suspend for the Observed region, for the Reactive
    /// region, and for another Active pass.
    observed: &'a [usize],
    reactive: &'a [usize],
    deferred: &'a [usize],
    /// Whether any kernel suspends for the Observed or Reactive region, and
    /// the blocks of those regions when one does.
    assertions: bool,
    observed_entry: Option<celox_sir::BlockId>,
    reactive_entry: Option<celox_sir::BlockId>,
    reactive_end: Option<celox_sir::BlockId>,
    slot_done: Option<celox_sir::BlockId>,
    /// Whether the unit takes host inputs through the native services.
    host_inputs: bool,
    /// The head of the slot loop.
    header: celox_sir::BlockId,
}

impl Plan {
    pub(super) fn generate(
        &self,
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
    ) -> Unit {
        self.generate_with_outlines(program, slots, None, 0)
    }

    pub(super) fn generate_with_outlines(
        &self,
        program: &LaidOutProgram,
        slots: &SchedulerSlots<AbsoluteAddr>,
        outlined: Option<&mut Outlines>,
        outline_base: u32,
    ) -> Unit {
        let schema = &program.runtime.runtime_schema;
        let edged = self.edged();
        let tracked = self.tracked();
        let clock_bits = self.clocks.len().next_multiple_of(64).max(64);
        let mut b = Builder::new();
        let comb = b.new_block();
        let native_nbas: HashSet<_> = self
            .nba_processes
            .iter()
            .copied()
            .filter(|&index| self.kernels[index].is_some())
            .collect();
        let nbas = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            .then_some(self.service_function)
            .flatten()
            .filter(|_| !schema.nba_sites.is_empty())
            .map(|service| nba::Sites::new(program, &native_nbas, service, &self.events));
        let mut g = Generator {
            inlined_instructions: 0,
            program,
            nbas: nbas.as_ref(),
            observers: self.observers.as_ref(),
            observer_service: self.service_function,
            outlined,
            outline_base,
            b,
            slots,
            finish_sites: &self.finish_sites,
            exits: Vec::new(),
            settle_reads: self.settle_reads.as_ref(),
            incremental: self.incremental.as_ref(),
            partitioned_glue: program
                .runtime
                .runtime_schema
                .settle_partitions
                .as_ref()
                .map(|_| (program, HashSet::default())),
            shared: program.shared_storage(),
            comb,
            continuations: Vec::new(),
            eager: None,
            eager_continuations: Vec::new(),
            four_state: program
                .layout()
                .four_state
                .then_some(&program.layout().is_4states),
            event_bits: self.event_bits,
            ready_bits: self
                .kernels
                .iter()
                .flatten()
                .count()
                .next_multiple_of(64)
                .max(64),
            cont_bits: program.layout().widths[&slots.cont],
            event_batch: {
                let capacity = program.layout().runtime_event_capacity;
                (capacity > 0
                    && (!schema.runtime_event_sites.is_empty()
                        || !self.service_processes.is_empty()))
                .then_some((capacity / 8).max(1) as u64)
            },
        };
        let mut guards = if cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && (self.cyclic || !self.service_processes.is_empty())
        {
            self.service_function
                .map(|service| ordered::Guards::new(&mut g, service))
        } else {
            None
        };
        let waits: Vec<(usize, &Wait)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| Some((index, kernel.as_ref()?.wait.as_ref()?)))
            .collect();
        // The kernels that delay, with the status slot of those that also
        // wait, which tells whether they wait for their wake time.
        let delays: Vec<(usize, Option<AbsoluteAddr>)> = self
            .kernels
            .iter()
            .enumerate()
            .filter_map(|(index, kernel)| {
                let kernel = kernel.as_ref().filter(|kernel| kernel.delays)?;
                let status = kernel
                    .wait
                    .is_some()
                    .then_some(schema.processes[index].status);
                Some((index, status))
            })
            .collect();
        // The bit of each kernel in the ready mask: its place among the
        // kernels the scheduler runs, in kernel order.
        let ordinals = ordinals(&self.kernels);
        let checks = waits_of(&self.kernels, &ordinals, schema);
        let status_kernels = |status| {
            self.kernels
                .iter()
                .enumerate()
                .filter(|(index, kernel)| {
                    kernel.is_some()
                        && returns_status(
                            &program.sir.processes[*index],
                            schema.processes[*index].status,
                            status,
                        )
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>()
        };
        let observed = status_kernels(ProcessStatus::AssertionObserved);
        let reactive = status_kernels(ProcessStatus::AssertionReactive);
        let deferred = status_kernels(ProcessStatus::DeferActive);
        let assertions = !observed.is_empty() || !reactive.is_empty();
        let observed_entry = assertions.then(|| g.b.new_block());
        let reactive_entry = assertions.then(|| g.b.new_block());
        let reactive_end = assertions.then(|| g.b.new_block());
        let slot_done = assertions.then(|| g.b.new_block());
        // The caller supplies the dirty flag. Timed runs must start dirty:
        // checkpoint restores and partial-slot exits need a fresh settle.
        // A host clock batch may instead enter with already settled inputs.
        let host_inputs = cfg!(all(target_arch = "x86_64", not(feature = "arm64-codegen")))
            && self.service_function.is_some();
        let header = g.b.new_block();
        let cx = Slot {
            program,
            slots,
            schema,
            edged: &edged,
            tracked: &tracked,
            clock_bits,
            waits: &waits,
            delays: &delays,
            ordinals: &ordinals,
            checks: &checks,
            observed: &observed,
            reactive: &reactive,
            deferred: &deferred,
            assertions,
            observed_entry,
            reactive_entry,
            reactive_end,
            slot_done,
            host_inputs,
            header,
        };
        let (time, started, run, stopped) = self.next_slot(&mut g, &cx, &nbas);

        let internal_exit = g.exit_block(SchedulerExit::Internal);
        let (host_entry, host_levels) =
            self.start_slot(&mut g, &cx, &nbas, &guards, time, run, internal_exit);
        let finish_exit = g.exit_block(SchedulerExit::Finish);
        // A host that watches every update sees each kernel run, the host
        // clocks a slot applies and each settle (IEEE 1800-2023 38.36.1).
        let updates = self.service_function.filter(|_| {
            program
                .runtime
                .runtime_schema
                .scheduler
                .as_ref()
                .is_some_and(|scheduler| scheduler.update_points)
        });
        let pick = g.b.new_block();
        let entry_done = pick;
        self.apply_host_clocks(
            &mut g,
            &cx,
            started,
            time,
            host_entry,
            host_levels,
            updates,
            finish_exit,
            entry_done,
        );

        let (pass_end, runs) = self.pick_kernel(&mut g, &cx, pick, internal_exit);
        self.run_kernels(
            &mut g,
            &cx,
            &mut guards,
            runs,
            time,
            pick,
            finish_exit,
            internal_exit,
            updates,
        );

        let (slot_end, domains, resume_domains) = self.settle_pass(
            &mut g,
            &cx,
            &mut guards,
            time,
            pass_end,
            host_entry,
            pick,
            internal_exit,
        );

        // The domains the edges trigger, then the NBA region.
        g.b.switch_to_block(domains);
        self.observe_slot(&mut g, &tracked, &checks, finish_exit, true);
        if !self.active.is_empty() || self.ordered.is_some() {
            self.batch_active(
                &mut g,
                &tracked,
                slot_end,
                finish_exit,
                &checks,
                pick,
                resume_domains.expect("active resume block"),
            );
        } else if !self.derived.is_empty()
            || !self.counted.is_empty()
            || self.events.iter().any(|event| event.any_settled)
        {
            // Unspecified domains use rising edges for scheduled writes, but
            // any change for later settled waves. Keep that ordering when
            // another clock's commit changes the domain's inputs.
            self.batch(&mut g, &tracked, slot_end, finish_exit, &checks);
        } else {
            self.simple_domains(
                &mut g,
                &cx,
                updates,
                slot_end,
                pick,
                finish_exit,
                internal_exit,
            );
        }

        self.rearm_clocks(&mut g, &cx, slot_end, started, time);
        self.drain_woken(&mut g, &cx, &guards, time, pick, internal_exit);
        self.assertion_regions(&mut g, &cx, pick);
        // The run returns after the slot that reaches its budget, or in
        // which its runtime events reach a batch, so that the host drains
        // them before the ring buffer overflows.
        let count = g.load(slots.count, SCHEDULER_TIME_WIDTH);
        let budget = g.load(slots.budget, SCHEDULER_TIME_WIDTH);
        let mut returns = g.binary(1, count, BinaryOp::GeU, budget);
        if let Some(batch) = g.event_batch {
            let events = g.load(slots.events, SCHEDULER_TIME_WIDTH);
            let batch = g.constant(batch, SCHEDULER_TIME_WIDTH);
            let full = g.binary(1, events, BinaryOp::GeU, batch);
            returns = g.binary(1, returns, BinaryOp::Or, full);
        }
        // A run that returns there has not judged its next slot.
        let unjudged = g.b.new_block();
        g.b.seal_block(SIRTerminator::Branch {
            cond: returns,
            true_block: (unjudged, Vec::new()),
            false_block: (header, Vec::new()),
        });
        g.b.switch_to_block(unjudged);
        let unknown = g.constant(celox_design::SCHEDULER_NEXT_UNJUDGED, SCHEDULER_TIME_WIDTH);
        g.store(slots.next, SCHEDULER_TIME_WIDTH, unknown);
        g.b.seal_block(SIRTerminator::Jump(stopped, Vec::new()));
        g.emit_settle(
            &program.sir.eval_comb,
            finish_exit,
            internal_exit,
            updates.map(|service| (service, &checks[..], &self.events[..])),
        );
        if let Some(lazy) = &self.lazy {
            g.emit_eager(&lazy.eager, finish_exit, internal_exit);
        }
        if let Some(guards) = guards {
            guards.emit(&mut g, internal_exit);
        }
        g.finish()
    }
}
