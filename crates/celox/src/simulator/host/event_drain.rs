//! Draining, peeking and queueing runtime events of a [`Simulator`].

use super::*;

/// The reading side of the runtime-event buffer of a [`Simulator`].
pub(crate) struct RuntimeEventStream {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: the runtime-event read sequence shared by the simulation thread and a RuntimeEventDrain handle that may drain on another thread"
    )]
    pub(super) read_seq: Arc<AtomicU64>,
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: a RuntimeEventDrain handle, possibly on another thread, marks itself active and the simulation thread checks it"
    )]
    pub(super) drain_active: Arc<AtomicBool>,
    /// Set while the active drain paces the simulation
    /// ([`RuntimeEventDrain::set_backpressure`]).
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: a RuntimeEventDrain handle, possibly on another thread, turns pacing on and the simulation thread reads it before each step"
    )]
    pub(super) backpressure: Arc<AtomicBool>,
    /// Whether a runtime-event site records heap handles
    /// (`sites_record_handles`).
    pub(super) handles: bool,
    pub(super) pacing: Arc<DrainPacing>,
    /// Runtime-event sequence up to which a timed simulation looked for
    /// `$finish` records.
    pub(crate) finish_scan_seq: u64,
}

impl RuntimeEventStream {
    #[expect(
        clippy::disallowed_types,
        reason = "cross-thread: creates the read sequence and flags shared with a RuntimeEventDrain handle that may run on another thread"
    )]
    pub(super) fn new(sites: &[RuntimeEventSite]) -> Self {
        Self {
            read_seq: Arc::new(AtomicU64::new(0)),
            drain_active: Arc::new(AtomicBool::new(false)),
            backpressure: Arc::new(AtomicBool::new(false)),
            handles: sites_record_handles(sites),
            pacing: Arc::default(),
            finish_scan_seq: 0,
        }
    }
}

impl<B: SimBackend> Simulator<B> {
    /// Drain runtime events, lazily evaluating dirty combinational logic.
    /// Fatal reports encountered during evaluation are returned as
    /// [`RuntimeEvent::Report`] records so the host can handle them.
    pub fn drain_runtime_events(&mut self) -> Vec<RuntimeEvent> {
        self.drain_runtime_events_with_context(RuntimeFormatContext::default())
    }

    pub fn drain_runtime_events_with_context(
        &mut self,
        ctx: RuntimeFormatContext<'_>,
    ) -> Vec<RuntimeEvent> {
        assert!(
            !self.runtime_events.drain_active.load(Ordering::Acquire),
            "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
        );
        self.settle_for_runtime_event_drain();
        // A fatal action is drained as a fatal report.
        let _ = self.end_untimed_slot();
        self.collect_formatted_runtime_events(ctx)
    }

    /// Settle dirty combinational logic before its events are drained.
    pub(crate) fn settle_for_runtime_event_drain(&mut self) {
        if self.dirty {
            self.eval_comb_for_runtime_event_drain().unwrap();
        }
    }

    /// Without a scheduler, a host call is a time slot: end it.
    pub(super) fn end_untimed_slot(&mut self) -> Result<(), RuntimeErrorCode> {
        if self.timed {
            return Ok(());
        }
        self.end_host_slot()
    }

    pub(crate) fn drain_runtime_events_deferred_with_context(
        &mut self,
        ctx: RuntimeFormatContext<'_>,
    ) -> Vec<RuntimeEvent> {
        assert!(
            !self.runtime_events.drain_active.load(Ordering::Acquire),
            "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
        );
        if !self.program.runtime_schema.comb_observers.is_empty() && self.dirty {
            self.eval_comb_for_runtime_event_drain().unwrap();
        }
        self.collect_formatted_runtime_events(ctx)
    }

    /// Drain runtime events while combinational logic is settled, as
    /// after a step of a timed simulation.
    pub(crate) fn drain_settled_runtime_events_with_context(
        &mut self,
        ctx: RuntimeFormatContext<'_>,
    ) -> Vec<RuntimeEvent> {
        assert!(
            !self.runtime_events.drain_active.load(Ordering::Acquire),
            "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
        );
        self.collect_formatted_runtime_events(ctx)
    }

    pub(super) fn eval_comb_for_runtime_event_drain(&mut self) -> Result<(), RuntimeErrorCode> {
        let (result, _) = self.eval_comb_capturing_events();
        self.check_comb_backend_result(result)?;
        self.dirty = false;
        Ok(())
    }

    pub(super) fn collect_formatted_runtime_events(
        &mut self,
        ctx: RuntimeFormatContext<'_>,
    ) -> Vec<RuntimeEvent> {
        let queue = Arc::clone(&self.pending_host_events);
        let mut pending = queue.lock().unwrap();
        // Most clock boundaries produce no reports. Preserve queued host
        // records, losses and deferred string snapshots, but avoid the
        // collector and formatter when neither queue has any new work.
        if pending.events.is_empty()
            && pending.missed == 0
            && pending.deferred.waiting.is_empty()
            && self.runtime_events.read_seq.load(Ordering::Acquire)
                == self.runtime_event_write_seq()
        {
            return Vec::new();
        }
        pending.extend(
            self.collect_backend_runtime_events(),
            self.backend.layout().runtime_event_capacity,
        );
        pending.resolve_deferred_handles(self.backend.heap());
        let events = pending.drain();
        drop(pending);
        events
            .into_iter()
            .filter_map(|raw| {
                render_raw_runtime_event(raw, &self.program.runtime_schema.runtime_event_sites, ctx)
            })
            .map(|event| untimed_report(event, self.timed, ctx))
            .collect()
    }

    /// Drain runtime events as [`Self::drain_runtime_events_deferred_with_context`]
    /// does, with the site of each. A record of `marker` gives the events
    /// recorded before it, since the previous marker, the simulation time
    /// of its argument; later events have the time of `ctx`.
    pub(crate) fn drain_marked_runtime_events(
        &mut self,
        ctx: RuntimeFormatContext<'_>,
        marker: usize,
    ) -> Vec<(Option<usize>, RuntimeEvent)> {
        assert!(
            !self.runtime_events.drain_active.load(Ordering::Acquire),
            "cannot use Simulator::drain_runtime_events while a RuntimeEventDrain is active",
        );
        if !self.program.runtime_schema.comb_observers.is_empty() && self.dirty {
            self.eval_comb_for_runtime_event_drain().unwrap();
        }
        let queue = Arc::clone(&self.pending_host_events);
        let mut pending = queue.lock().unwrap();
        if pending.events.is_empty()
            && pending.missed == 0
            && pending.deferred.waiting.is_empty()
            && self.runtime_events.read_seq.load(Ordering::Acquire)
                == self.runtime_event_write_seq()
        {
            return Vec::new();
        }
        pending.extend(
            self.collect_backend_runtime_events(),
            self.backend.layout().runtime_event_capacity,
        );
        pending.resolve_deferred_handles(self.backend.heap());
        let raw = pending.drain();
        drop(pending);
        let sites = &self.program.runtime_schema.runtime_event_sites;
        let mut events = Vec::with_capacity(raw.len());
        let mut unmarked = Vec::new();
        for raw in raw {
            let site = match &raw {
                RawRuntimeEvent::Event { site_id, args } if *site_id == marker => {
                    let time = args
                        .first()
                        .and_then(|time| time.values.first().copied())
                        .unwrap_or(0);
                    let marked = RuntimeFormatContext {
                        tb_time: Some(time),
                        ..ctx
                    };
                    for (site, raw) in unmarked.drain(..) {
                        events.extend(
                            render_raw_runtime_event(raw, sites, marked)
                                .map(|event| (site, untimed_report(event, self.timed, marked))),
                        );
                    }
                    continue;
                }
                RawRuntimeEvent::Event { site_id, .. } => Some(*site_id),
                _ => None,
            };
            unmarked.push((site, raw));
        }
        for (site, raw) in unmarked {
            events.extend(
                render_raw_runtime_event(raw, sites, ctx)
                    .map(|event| (site, untimed_report(event, self.timed, ctx))),
            );
        }
        events
    }

    pub(super) fn queue_host_runtime_event(
        &mut self,
        site: RuntimeEventSite,
        args: Vec<RuntimeEventArgValue>,
    ) {
        let queue = Arc::clone(&self.pending_host_events);
        let mut pending = queue.lock().unwrap();
        let capacity = self.backend.layout().runtime_event_capacity;
        pending.extend(self.collect_backend_runtime_events(), capacity);
        pending.resolve_deferred_handles(self.backend.heap());
        pending.push(
            RawRuntimeEvent::Host {
                site: Box::new(site),
                args,
            },
            capacity,
        );
    }

    /// The runtime events [`Self::drain_runtime_events`] would return
    /// next, without taking them.
    pub(crate) fn peek_runtime_events(&mut self) -> Vec<RuntimeEvent> {
        self.stash_runtime_events();
        let raw = self.pending_host_events.lock().unwrap().clone().drain();
        raw.into_iter()
            .filter_map(|raw| {
                render_raw_runtime_event(
                    raw,
                    &self.program.runtime_schema.runtime_event_sites,
                    RuntimeFormatContext::default(),
                )
            })
            .collect()
    }

    pub(super) fn collect_backend_runtime_events(&mut self) -> Vec<RawRuntimeEvent> {
        // String arguments are read now, while the heap still holds them.
        let mut events = self.collect_backend_raw_runtime_events();
        for event in &mut events {
            event.resolve_handles(self.backend.heap());
        }
        events
    }

    pub(super) fn collect_backend_raw_runtime_events(&mut self) -> Vec<RawRuntimeEvent> {
        let layout = self.backend.layout();
        let mut read_seq = self.runtime_events.read_seq.load(Ordering::Acquire);
        if let Some(buffer) = self.backend.runtime_event_buffer() {
            let events = collect_runtime_events(
                layout,
                &self.program.runtime_schema.runtime_event_sites,
                &mut read_seq,
                buffer.byte_size(),
                |offset| buffer.read_u64(offset),
                |offset| buffer.load_atomic_u64(offset, std::sync::atomic::Ordering::Acquire),
            );
            self.runtime_events
                .read_seq
                .store(read_seq, Ordering::Release);
            events
        } else {
            let (ptr, size) = self.backend.runtime_event_buffer_as_ptr();
            let read_u64 = |offset: usize| -> u64 {
                unsafe { std::ptr::read_volatile(ptr.add(offset) as *const u64) }
            };
            let events = collect_runtime_events(
                layout,
                &self.program.runtime_schema.runtime_event_sites,
                &mut read_seq,
                size,
                read_u64,
                read_u64,
            );
            self.runtime_events
                .read_seq
                .store(read_seq, Ordering::Release);
            events
        }
    }

    /// Whether the design wrote a `$finish` record since the last call.
    pub(crate) fn take_runtime_finish(&mut self) -> bool {
        if !self.finish_sites {
            return false;
        }
        let write_seq = self.runtime_event_write_seq();
        if write_seq == self.runtime_events.finish_scan_seq {
            return false;
        }
        let events = self.peek_backend_runtime_events_from(self.runtime_events.finish_scan_seq);
        self.runtime_events.finish_scan_seq = write_seq;
        let sites = &self.program.runtime_schema.runtime_event_sites;
        events.iter().any(|event| {
            matches!(
                event,
                RawRuntimeEvent::Event { site_id, .. }
                    if sites.get(*site_id).is_some_and(|site| {
                        matches!(site.kind, RuntimeEventKind::Finish)
                    })
            )
        })
    }

    /// Start looking for `$finish` records at the current sequence.
    pub(crate) fn skip_runtime_finish(&mut self) {
        if self.finish_sites {
            self.runtime_events.finish_scan_seq = self.runtime_event_write_seq();
        }
    }

    pub(super) fn runtime_event_write_seq(&self) -> u64 {
        if self.program.runtime_schema.runtime_event_sites.is_empty() {
            return 0;
        }
        if let Some(buffer) = self.backend.runtime_event_buffer() {
            buffer.load_atomic_u64(0, std::sync::atomic::Ordering::Acquire)
        } else {
            let (ptr, _size) = self.backend.runtime_event_buffer_as_ptr();
            unsafe { std::ptr::read_volatile(ptr as *const u64) }
        }
    }

    pub(super) fn peek_backend_runtime_events_from(&self, read_seq: u64) -> Vec<RawRuntimeEvent> {
        let mut read_seq = read_seq;
        let layout = self.backend.layout();
        if let Some(buffer) = self.backend.runtime_event_buffer() {
            collect_runtime_events(
                layout,
                &self.program.runtime_schema.runtime_event_sites,
                &mut read_seq,
                buffer.byte_size(),
                |offset| buffer.read_u64(offset),
                |offset| buffer.load_atomic_u64(offset, std::sync::atomic::Ordering::Acquire),
            )
        } else {
            let (ptr, size) = self.backend.runtime_event_buffer_as_ptr();
            let read_u64 = |offset: usize| -> u64 {
                unsafe { std::ptr::read_volatile(ptr.add(offset) as *const u64) }
            };
            collect_runtime_events(
                layout,
                &self.program.runtime_schema.runtime_event_sites,
                &mut read_seq,
                size,
                read_u64,
                read_u64,
            )
        }
    }

    /// Whether a [`RuntimeEventDrain`] is active.
    pub(crate) fn runtime_event_drain_active(&self) -> bool {
        self.runtime_events.drain_active.load(Ordering::Acquire)
    }

    pub fn runtime_event_drain(&mut self) -> Option<RuntimeEventDrain> {
        let buffer = self.backend.runtime_event_buffer()?;
        self.runtime_events
            .drain_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(RuntimeEventDrain {
            timed: self.timed,
            buffer,
            layout: self.backend.layout().clone(),
            sites: self.program.runtime_schema.runtime_event_sites.clone(),
            pending_host_events: Arc::clone(&self.pending_host_events),
            read_seq: self.runtime_events.read_seq.load(Ordering::Acquire),
            shared_read_seq: Arc::clone(&self.runtime_events.read_seq),
            active: Arc::clone(&self.runtime_events.drain_active),
            backpressure: Arc::clone(&self.runtime_events.backpressure),
            handles: self.runtime_events.handles,
            pacing: Arc::clone(&self.runtime_events.pacing),
        })
    }

    /// Wait until a drain that paces the simulation has taken every
    /// record (see [`RuntimeEventDrain::set_backpressure`]), those in the
    /// ring buffer and those the host moved into its bounded queue, so
    /// that the step keeps every record up to the buffer's capacity.
    pub(crate) fn await_runtime_event_room(&mut self) {
        let pacing = Arc::clone(&self.runtime_events.pacing);
        let mut registered = false;
        while self.runtime_events.backpressure.load(Ordering::Acquire)
            && self.runtime_events.drain_active.load(Ordering::Acquire)
        {
            // The drain takes records with handles from the host queue
            // once this thread resolved them there.
            if self.runtime_events.handles {
                self.stash_runtime_events();
            }
            let queued = self.pending_host_events.lock().unwrap().events.len();
            let read = self.runtime_events.read_seq.load(Ordering::Acquire);
            if queued == 0 && self.runtime_event_write_seq() <= read {
                break;
            }
            // The drain wakes this thread once it took events or ended;
            // check again after registering, so that no wakeup is missed.
            if !registered {
                DrainPacing::register(&pacing.simulation);
                registered = true;
                continue;
            }
            DrainPacing::wake(&pacing.drain);
            std::thread::park();
        }
    }

    pub(in crate::simulator) fn settle_dirty_for_runtime_event_drain(&mut self) {
        if self.runtime_events.drain_active.load(Ordering::Acquire) {
            self.eval_comb_for_runtime_event_drain().unwrap();
            // A fatal action is drained as a fatal report.
            let _ = self.end_untimed_slot();
        }
    }
}
