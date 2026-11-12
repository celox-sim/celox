//! Deferred assertions and sampled values: the queues of pending reports and the end of a host time slot.

use super::*;

/// What the end of a time slot has to do.
pub(crate) struct SlotEnd {
    /// Deferred assertions or sampled copies have work when a host
    /// call's time slot is over.
    pub(crate) work: bool,
    /// The design has deferred assertions, whose actions mature at the
    /// end of each time slot.
    pub(crate) deferred_assertions: bool,
    /// Sampled variables and their copies
    /// (`RuntimeSchema::sample_shadows`).
    pub(super) sample_shadows: Vec<(SignalRef, SignalRef)>,
    pub(crate) postponed_output: bool,
}

impl SlotEnd {
    pub(super) fn new<B: SimBackend>(
        backend: &B,
        schema: &celox_design::RuntimeSchema<crate::ir::AbsoluteAddr>,
        deferred_assertions: bool,
    ) -> Self {
        let sample_shadows: Vec<(SignalRef, SignalRef)> = schema
            .sample_shadows
            .iter()
            .map(|(variable, copy)| {
                (
                    backend.resolve_signal(variable),
                    backend.resolve_signal(copy),
                )
            })
            .collect();
        let postponed_output = schema.host_request_sites.iter().any(|site| {
            site.kind == celox_design::HostRequestKind::FileIo
                && (16..=20).contains(&site.operation)
        });
        Self {
            work: postponed_output || deferred_assertions || !sample_shadows.is_empty(),
            deferred_assertions,
            sample_shadows,
            postponed_output,
        }
    }
}

/// Host identities are deliberately outside core checkpoints. A restored
/// activation slot must not flush reports left by a different execution.
#[derive(Clone, Default)]
pub(in crate::simulator) struct DeferredOwnerContext {
    pub(super) ordinary: u64,
    pub(super) current: Option<u64>,
    pub(super) processes: crate::HashMap<usize, u64>,
}

impl DeferredOwnerContext {
    pub(super) fn owner(&self) -> u64 {
        self.current.unwrap_or(self.ordinary)
    }
}

/// Host-owned continuation of the backend ring and postponed work.
/// Records remain unformatted until the eventual consumer supplies context.
#[derive(Clone, Default)]
pub(super) struct PendingRuntimeEvents {
    pub(super) events: std::collections::VecDeque<RawRuntimeEvent>,
    pub(super) missed: u64,
    pub(super) deferred: DeferredQueues,
    pub(super) deferred_owners: DeferredOwnerContext,
    pub(super) next_deferred_owner: u64,
    pub(super) strobes: Vec<StrobeCall>,
    pub(super) monitors: MonitorState,
}

#[derive(Clone)]
pub(super) struct DeferredReport {
    /// The flush-point site whose queue holds it, or [`PROCESS_QUEUE`].
    pub(super) queue: u32,
    pub(super) owner: u64,
    pub(super) region: celox_design::AssertionDeferral,
    pub(super) event: RawRuntimeEvent,
}

/// The queue of the action of a function unit: that of the process the
/// unit runs in, whichever its flush-point site (see
/// [`celox_design::DeferredSite::ProcessAction`]).
pub(super) const PROCESS_QUEUE: u32 = u32::MAX;

#[derive(Clone)]
pub(super) struct DeferredLoss {
    pub(super) region: celox_design::AssertionDeferral,
    pub(super) count: u64,
}

/// The actions of deferred assertions that wait in the queue of their
/// process until they mature (IEEE 1800-2023 16.4.1).
#[derive(Clone, Default)]
pub(super) struct DeferredQueues {
    /// How the events of each site are deferred, and whether the site is
    /// fatal, by site id. Empty without deferred assertions.
    pub(super) sites: Vec<Option<(celox_design::DeferredSite, bool)>>,
    /// Waiting actions in issue order, with their queue and region.
    pub(super) waiting: Vec<DeferredReport>,
    /// Events lost to the ring since the last maturity, in a design with
    /// fatal deferred actions: they may have held one. Cancellation must
    /// discard only losses belonging to that execution, too.
    pub(super) missed: crate::HashMap<u64, DeferredLoss>,
}

impl PendingRuntimeEvents {
    pub(super) fn resolve_deferred_handles(&mut self, heap: &celox_heap::Heap) {
        // The raw drain can enqueue unresolved string arguments, but it
        // cannot mature them. Resolve before the owning thread changes
        // heap state or publishes the actions.
        for report in &mut self.deferred.waiting {
            report.event.resolve_handles(heap);
        }
    }

    pub(super) fn new_deferred_owner(&mut self) -> u64 {
        self.next_deferred_owner = self
            .next_deferred_owner
            .checked_add(1)
            .expect("deferred assertion owner identity exhausted");
        self.next_deferred_owner
    }

    pub(super) fn enter_deferred_process(&mut self, process: usize) {
        debug_assert!(self.deferred_owners.current.is_none());
        let owner = if let Some(&owner) = self.deferred_owners.processes.get(&process) {
            owner
        } else {
            let owner = self.new_deferred_owner();
            self.deferred_owners.processes.insert(process, owner);
            owner
        };
        self.deferred_owners.current = Some(owner);
    }

    pub(super) fn cancel_deferred_process(&mut self, process: usize) {
        if let Some(owner) = self.deferred_owners.processes.remove(&process) {
            self.deferred.waiting.retain(|report| report.owner != owner);
            self.deferred.missed.remove(&owner);
        }
    }

    pub(super) fn replace_deferred_execution(&mut self) {
        self.deferred_owners = DeferredOwnerContext {
            ordinary: self.new_deferred_owner(),
            ..Default::default()
        };
    }

    pub(super) fn extend(
        &mut self,
        events: impl IntoIterator<Item = RawRuntimeEvent>,
        capacity: usize,
    ) {
        for event in events {
            self.push(event, capacity);
        }
    }

    /// Queue the event, unless it is the action of a deferred assertion,
    /// which waits, or a flush point, which discards the actions waiting
    /// in its queue (16.4.2).
    pub(super) fn push(&mut self, event: RawRuntimeEvent, capacity: usize) {
        if let RawRuntimeEvent::Event { site_id, .. } = &event
            && let Some(Some((deferred, _))) = self.deferred.sites.get(*site_id)
        {
            let site = *site_id as u32;
            match *deferred {
                celox_design::DeferredSite::FlushPoint => self.flush(site),
                celox_design::DeferredSite::ProcessFlushPoint => self.flush_process(),
                celox_design::DeferredSite::Action { queue, region } => {
                    self.deferred.waiting.push(DeferredReport {
                        queue,
                        owner: self.deferred_owners.owner(),
                        region,
                        event,
                    });
                }
                celox_design::DeferredSite::ProcessAction { region } => {
                    self.deferred.waiting.push(DeferredReport {
                        queue: PROCESS_QUEUE,
                        owner: self.deferred_owners.owner(),
                        region,
                        event,
                    });
                }
            }
            return;
        }
        if let RawRuntimeEvent::Missed { count } = event
            && self
                .deferred
                .sites
                .iter()
                .flatten()
                .any(|&(_, fatal)| fatal)
        {
            // Lost records have no site identity. Fail conservatively in
            // the earliest region that could have contained a fatal action.
            let region = if self.deferred.sites.iter().flatten().any(|(site, fatal)| {
                *fatal
                    && matches!(
                        site,
                        celox_design::DeferredSite::Action {
                            region: celox_design::AssertionDeferral::Observed,
                            ..
                        } | celox_design::DeferredSite::ProcessAction {
                            region: celox_design::AssertionDeferral::Observed,
                        }
                    )
            }) {
                celox_design::AssertionDeferral::Observed
            } else {
                celox_design::AssertionDeferral::Final
            };
            self.deferred
                .missed
                .entry(self.deferred_owners.owner())
                .or_insert(DeferredLoss { region, count: 0 })
                .count += count;
        }
        self.push_ready(event, capacity);
    }

    /// Discard the actions waiting in queue `queue`.
    pub(super) fn flush(&mut self, queue: u32) {
        let owner = self.deferred_owners.owner();
        // The actions of the units the process ran wait in its queue too.
        self.deferred.waiting.retain(|report| {
            report.owner != owner || (report.queue != queue && report.queue != PROCESS_QUEUE)
        });
        // A dynamic execution has exactly its process's queue. The
        // ordinary context may mix unrelated comb/FF queues, so losses
        // there must remain conservatively fatal until maturity.
        if self.deferred_owners.current.is_some() {
            self.deferred.missed.remove(&owner);
        }
    }

    /// A flush point of the process running now, whatever its queue: a
    /// function unit's, which serves every process (16.4.2). Only a
    /// dynamic execution has one process.
    pub(super) fn flush_process(&mut self) {
        if self.deferred_owners.current.is_none() {
            return;
        }
        let owner = self.deferred_owners.owner();
        self.deferred.waiting.retain(|report| report.owner != owner);
        self.deferred.missed.remove(&owner);
    }

    /// Mature one region. Matured actions leave the process queue and
    /// cannot be flushed by activity in a later region (16.4.1).
    pub(super) fn mature(
        &mut self,
        region: celox_design::AssertionDeferral,
        capacity: usize,
    ) -> Option<RawRuntimeEvent> {
        let waiting = std::mem::take(&mut self.deferred.waiting);
        let mut missed = 0;
        self.deferred.missed.retain(|_, loss| {
            if loss.region == region {
                missed += loss.count;
                false
            } else {
                true
            }
        });
        let fatal = (missed > 0).then_some(RawRuntimeEvent::Missed { count: missed });
        for report in waiting {
            if report.region != region {
                self.deferred.waiting.push(report);
                continue;
            }
            self.push_ready(report.event.clone(), capacity);
            if let RawRuntimeEvent::Event { site_id, .. } = &report.event
                && let Some(Some((_, true))) = self.deferred.sites.get(*site_id)
            {
                // No further actions run after a known fatal action.
                self.deferred.waiting.clear();
                self.deferred.missed.clear();
                return fatal.or(Some(report.event));
            }
        }
        fatal
    }

    pub(super) fn push_ready(&mut self, event: RawRuntimeEvent, capacity: usize) {
        if let RawRuntimeEvent::Missed { count } = event {
            // Ring overwrite means every older pending record also lies
            // outside the latest `capacity` records of the combined stream.
            self.missed += count + self.events.len() as u64;
            self.events.clear();
        } else if capacity == 0 {
            self.missed += 1;
        } else {
            if self.events.len() == capacity {
                self.events.pop_front();
                self.missed += 1;
            }
            self.events.push_back(event);
        }
    }

    pub(super) fn drain(&mut self) -> Vec<RawRuntimeEvent> {
        let mut events = Vec::with_capacity(self.events.len() + usize::from(self.missed > 0));
        if self.missed > 0 {
            events.push(RawRuntimeEvent::Missed {
                count: std::mem::take(&mut self.missed),
            });
        }
        events.extend(self.events.drain(..));
        events
    }
}

impl<B: SimBackend> Simulator<B> {
    /// A host call without a scheduler is a time slot, which is over:
    /// deferred assertions mature, then the sampled copies are made.
    #[inline]
    pub(super) fn end_host_slot(&mut self) -> Result<(), RuntimeErrorCode> {
        if !self.slot_end.work {
            return Ok(());
        }
        self.finish_host_slot()
    }

    /// A host call without a scheduler begins a time slot. Logic that
    /// host writes since the previous call left dirty settles first, in
    /// the previous slot, which then ends: the call's domains sample the
    /// values it settled to, and settles within the call do not change
    /// them.
    #[inline]
    pub(super) fn begin_host_slot(&mut self) -> Result<(), RuntimeErrorCode> {
        if !self.slot_end.work || !self.dirty || self.timed {
            return Ok(());
        }
        self.settle_host_slot()
    }

    #[cold]
    #[inline(never)]
    pub(super) fn settle_host_slot(&mut self) -> Result<(), RuntimeErrorCode> {
        self.eval_comb_checked()?;
        self.dirty = false;
        self.finish_host_slot()
    }

    #[cold]
    #[inline(never)]
    pub(super) fn finish_host_slot(&mut self) -> Result<(), RuntimeErrorCode> {
        self.mature_deferred_assertions()?;
        self.sample_values();
        Ok(())
    }

    /// Before the first time slot, start the `$past` histories and the
    /// sampled copies with the values of their variables, the default
    /// sampled values (IEEE 1800-2023 16.5.1).
    pub(crate) fn initialize_copies(&mut self) {
        if !self.slot_end.work && self.program.runtime_schema.initial_copies.is_empty() {
            return;
        }
        for (variable, copy) in self.program.runtime_schema.initial_copies.clone() {
            let variable = self.backend.resolve_signal(&variable);
            let copy = self.backend.resolve_signal(&copy);
            celox_runtime::copy_signal(&mut self.backend, variable, copy);
        }
        self.sample_values();
    }

    /// Copy each sampled variable into its copy (IEEE 1800-2023
    /// 16.5.1).
    pub(crate) fn sample_values(&mut self) {
        for &(variable, copy) in &self.slot_end.sample_shadows {
            celox_runtime::copy_signal(&mut self.backend, variable, copy);
        }
    }

    #[cfg(test)]
    pub(crate) fn sampled_copy_count(&self) -> usize {
        self.slot_end.sample_shadows.len()
    }

    /// The time slot ends: the actions of deferred assertions that their
    /// process did not flush mature, `#0` before `final` (IEEE 1800-2023
    /// 16.4). A fatal one ends the simulation with an error.
    pub(crate) fn mature_deferred_assertions(&mut self) -> Result<(), RuntimeErrorCode> {
        self.mature_deferred_assertion_region(celox_design::AssertionDeferral::Observed)?;
        self.mature_deferred_assertion_region(celox_design::AssertionDeferral::Final)
    }

    pub(crate) fn mature_deferred_assertion_region(
        &mut self,
        region: celox_design::AssertionDeferral,
    ) -> Result<(), RuntimeErrorCode> {
        // Terminal output draining must not execute actions left in the
        // Observed/Postponed queues after final procedures (9.2.3).
        if self.frozen || !self.slot_end.deferred_assertions {
            return Ok(());
        }
        self.stash_runtime_events();
        let capacity = self.backend.layout().runtime_event_capacity;
        let fatal = self
            .pending_host_events
            .lock()
            .unwrap()
            .mature(region, capacity);
        if let Some(RawRuntimeEvent::Missed { count }) = fatal {
            return Err(RuntimeErrorCode::Runtime {
                message: format!(
                    "missed {count} runtime events, which may have held a deferred $fatal"
                ),
                signals: Vec::new(),
            });
        }
        let Some(RuntimeEvent::Report(report)) = fatal.and_then(|raw| {
            render_raw_runtime_event(
                raw,
                &self.program.runtime_schema.runtime_event_sites,
                RuntimeFormatContext::default(),
            )
        }) else {
            return Ok(());
        };
        Err(RuntimeErrorCode::Fatal {
            message: message_diagnostic(&report.message),
            signals: Vec::new(),
        })
    }

    /// Move the events in the backend's buffer to the queue that
    /// [`Self::drain_runtime_events`] reads.
    pub(crate) fn stash_runtime_events(&mut self) {
        // The raw drain shares this lock: consume and attribute records
        // atomically, before the current process owner can change.
        let queue = Arc::clone(&self.pending_host_events);
        let mut pending = queue.lock().unwrap();
        let events = self.collect_backend_runtime_events();
        let capacity = self.backend.layout().runtime_event_capacity;
        pending.extend(events, capacity);
        pending.resolve_deferred_handles(self.backend.heap());
    }

    pub(crate) fn enter_deferred_process(&mut self, process: usize) {
        if self.slot_end.deferred_assertions {
            self.stash_runtime_events();
            self.pending_host_events
                .lock()
                .unwrap()
                .enter_deferred_process(process);
        }
    }

    pub(crate) fn leave_deferred_process(&mut self) {
        if self.slot_end.deferred_assertions {
            self.stash_runtime_events();
            self.pending_host_events
                .lock()
                .unwrap()
                .deferred_owners
                .current = None;
        }
    }

    pub(crate) fn finish_deferred_process(&mut self, process: usize) {
        if self.slot_end.deferred_assertions {
            self.pending_host_events
                .lock()
                .unwrap()
                .deferred_owners
                .processes
                .remove(&process);
        }
    }

    pub(crate) fn cancel_deferred_process(&mut self, process: usize) {
        if self.slot_end.deferred_assertions {
            self.pending_host_events
                .lock()
                .unwrap()
                .cancel_deferred_process(process);
        }
    }

    pub(in crate::simulator) fn replace_deferred_execution(&mut self) {
        if self.slot_end.postponed_output {
            let mut pending = self.pending_host_events.lock().unwrap();
            pending.strobes.clear();
            pending.monitors = MonitorState::default();
        }
        if self.slot_end.deferred_assertions {
            self.stash_runtime_events();
            self.pending_host_events
                .lock()
                .unwrap()
                .replace_deferred_execution();
        }
    }

    pub(in crate::simulator) fn deferred_owner_context(&self) -> DeferredOwnerContext {
        self.pending_host_events
            .lock()
            .unwrap()
            .deferred_owners
            .clone()
    }

    pub(in crate::simulator) fn restore_deferred_owner_context(
        &mut self,
        owners: DeferredOwnerContext,
    ) {
        self.stash_runtime_events();
        self.pending_host_events.lock().unwrap().deferred_owners = owners;
    }
}
