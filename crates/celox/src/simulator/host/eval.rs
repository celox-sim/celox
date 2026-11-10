//! Combinational evaluation and the checked execution of processes and flip-flops.

use super::*;

impl<B: SimBackend> Simulator<B> {
    pub(super) fn check_comb_backend_result(
        &self,
        result: Result<(), RuntimeErrorCode>,
    ) -> Result<(), RuntimeErrorCode> {
        // The status itself identifies a fatal capture even when its
        // record has been overwritten in the bounded event buffer.
        match result {
            Err(RuntimeErrorCode::DetectedTrueLoopCode(code))
                if self.comb_fatal_site(code).is_some() =>
            {
                Ok(())
            }
            other => other.map_err(|error| self.decorate_runtime_error(error)),
        }
    }

    pub(super) fn comb_fatal_site(&self, code: i64) -> Option<&RuntimeEventSite> {
        let site = self
            .program
            .runtime_schema
            .runtime_event_sites
            .get(celox_runtime::comb_fatal_site(code)? as usize)?;
        site.kind.is_fatal().then_some(site)
    }

    pub(crate) fn eval_comb_checked(&mut self) -> Result<(), RuntimeErrorCode> {
        if self.frozen || self.declaration_phase {
            return Ok(());
        }
        let (result, events) = self.eval_comb_capturing_events();
        let fatal_status = matches!(
            &result,
            Err(RuntimeErrorCode::DetectedTrueLoopCode(code)) if self.comb_fatal_site(*code).is_some()
        );
        // Check the original backend code before formatting fatal records:
        // another parallel lane may have reported a loop or internal error.
        self.check_comb_backend_result(result)?;
        if let Some(error) = self.fatal_comb_capture_error(&events) {
            return Err(error);
        }
        if fatal_status {
            return Err(RuntimeErrorCode::Fatal {
                message: "Fatal assertion (runtime event record overwritten)".to_string(),
                signals: Vec::new(),
            });
        }
        Ok(())
    }

    pub(super) fn eval_comb_capturing_events(
        &mut self,
    ) -> (Result<(), RuntimeErrorCode>, Vec<RawRuntimeEvent>) {
        // Event-drain writes use this entry directly instead of the
        // checked facade. Leave observers and runtime events untouched.
        // A boundary observer may have marked the state dirty immediately
        // before freezing; draining output must not activate HDL afterwards.
        if self.frozen || self.declaration_phase {
            return (Ok(()), Vec::new());
        }
        if self.program.runtime_schema.runtime_event_sites.is_empty() {
            return (self.backend.eval_comb(), Vec::new());
        }
        let runtime_event_start_seq = self.runtime_event_write_seq();
        if self.program.runtime_schema.comb_observers.is_empty() {
            let result = self.backend.eval_comb();
            let events = self.peek_backend_runtime_events_from(runtime_event_start_seq);
            return (result, events);
        }

        let mut snapshots = std::mem::take(&mut self.comb_observer_scratch);
        self.comb_observer_reads
            .snapshot(&self.backend, &mut snapshots);
        let active_sites = crate::comb_activation::active_sites(
            &self.program.runtime_schema.comb_observers,
            self.program.runtime_schema.runtime_event_sites.len(),
            &self.comb_observer_reads,
            &snapshots,
            &self.comb_observer_snapshots,
            self.comb_observer_initial_eval,
        );
        if !self.comb_deferred_groups.is_empty() {
            self.flush_comb_deferred(&active_sites);
        }
        self.backend.set_comb_capture_event_enabled(&active_sites);
        let eval_result = self.backend.eval_comb();
        if !self.comb_deferred_groups.is_empty() {
            self.flush_comb_rerun_deferred(&active_sites);
        }
        self.comb_observer_reads
            .snapshot(&self.backend, &mut snapshots);
        let runtime_events = self.peek_backend_runtime_events_from(runtime_event_start_seq);
        self.backend.set_comb_capture_event_enabled(&vec![
            false;
            self.program
                .runtime_schema
                .runtime_event_sites
                .len()
        ]);
        self.comb_observer_scratch =
            std::mem::replace(&mut self.comb_observer_snapshots, snapshots);
        self.comb_observer_initial_eval = false;
        (eval_result, runtime_events)
    }

    /// The combinational processes that run again flush the actions of
    /// their deferred assertions, after the events issued before.
    #[cold]
    pub(super) fn flush_comb_deferred(&mut self, active_sites: &[bool]) {
        self.stash_runtime_events();
        let mut pending = self.pending_host_events.lock().unwrap();
        for &(group, site) in &self.comb_deferred_groups {
            if active_sites[site] {
                pending.flush(group);
            }
        }
    }

    /// A combinational process that runs again because the evaluation
    /// changed its inputs, which the backend enables it for, flushes the
    /// actions waiting from before the evaluation. Its new actions are
    /// still in the backend's buffer.
    #[cold]
    pub(super) fn flush_comb_rerun_deferred(&mut self, active_sites: &[bool]) {
        let enabled = self.backend.comb_capture_event_enabled();
        let mut pending = self.pending_host_events.lock().unwrap();
        for &(group, site) in &self.comb_deferred_groups {
            if !active_sites[site] && enabled.get(site).copied().unwrap_or(false) {
                pending.flush(group);
            }
        }
    }

    pub(in crate::simulator) fn snapshot_all_comb_observers(
        &self,
    ) -> Vec<crate::comb_activation::ObserverSnapshot> {
        self.comb_observer_reads.take(&self.backend)
    }

    pub(super) fn fatal_comb_capture_error(
        &self,
        events: &[RawRuntimeEvent],
    ) -> Option<RuntimeErrorCode> {
        events.iter().find_map(|raw| {
            let RawRuntimeEvent::Event { site_id, args } = raw else {
                return None;
            };
            let site = self
                .program
                .runtime_schema
                .runtime_event_sites
                .get(*site_id)?;
            // A deferred `$fatal` ends the simulation when it matures.
            if !site.kind.is_fatal() || site.deferred.is_some() {
                return None;
            }
            Some(RuntimeErrorCode::Fatal {
                message: message_diagnostic(&render_runtime_event_message(
                    site,
                    args,
                    RuntimeFormatContext::default(),
                )),
                signals: Vec::new(),
            })
        })
    }

    /// Keep a fatal report distinct from ordinary runtime faults before its
    /// backend status is decorated into a diagnostic.
    pub(super) fn checked_kernel_result(
        &self,
        result: Result<(), RuntimeErrorCode>,
        start: u64,
    ) -> Result<(), RuntimeErrorCode> {
        result.map_err(|error| {
                if self.program.runtime_schema.runtime_event_sites.is_empty() {
                    return self.decorate_runtime_error(error);
                }
                let comb_fatal = matches!(&error,
                    RuntimeErrorCode::DetectedTrueLoopCode(code) if self.comb_fatal_site(*code).is_some());
                self.fatal_comb_capture_error(&self.peek_backend_runtime_events_from(start))
                    .unwrap_or_else(|| if comb_fatal {
                        RuntimeErrorCode::Fatal {
                            message: "Fatal assertion (runtime event record overwritten)".into(),
                            signals: Vec::new(),
                        }
                    } else { self.decorate_runtime_error(error) })
            })
    }

    pub(crate) fn set_native_region_entry(&mut self, pending: bool) {
        self.native_arena.header.region_entry = u64::from(pending);
    }

    pub(crate) fn put_native_host_inputs(
        &mut self,
        pending: bool,
        writes: Vec<(SignalRef, (num_bigint::BigUint, num_bigint::BigUint))>,
    ) {
        if let Some(metadata) = &self.native_file_io {
            native_io::pack_host_inputs(&self.backend, metadata, &mut self.native_arena, writes);
        }
        self.native_arena.header.host_entry = u64::from(pending);
    }

    pub(crate) fn has_native_scheduler_services(&self) -> bool {
        self.native_file_io.is_some()
    }

    pub(crate) fn run_process_checked(&mut self, index: usize) -> Result<(), RuntimeErrorCode> {
        let start = self.runtime_event_write_seq();
        // The scheduler is a generated unit (`scheduler::attach`).
        let generated = index >= self.program.runtime_schema.processes.len();
        let native_observers = self.native_file_io.is_some()
            && generated
            && !self.program.runtime_schema.comb_observers.is_empty();
        if native_observers {
            self.comb_observer_reads.pack(
                &self.comb_observer_snapshots,
                &mut self.native_arena.observers,
            );
            self.native_arena.header.observers = self.native_arena.observers.as_mut_ptr();
            self.native_arena.header.observer_initial = u64::from(self.comb_observer_initial_eval);
            self.native_arena.header.observer_evaluating = 0;
        }
        // A kernel with value change points shows its updates to the
        // observer through the host context, outside compiled runs too.
        let result = if generated || (index >= self.scoped_from && self.value_change_kernels[index])
        {
            if let Some(metadata) = &self.native_file_io {
                let prepared = self
                    .slot_observer
                    .as_ref()
                    .and_then(|_| self.backend.prepare_process(index));
                let (state, state_size) = self.backend.memory_as_ptr();
                let mut observer = native_io::SlotService {
                    backend: &mut self.backend,
                    observer: self.slot_observer.as_deref_mut(),
                    watch: &mut self.host_watch,
                    dirty: &mut self.dirty,
                    changed: &mut self.value_changed,
                };
                let mut scope = native_io::Scope {
                    arena: &mut self.native_arena,
                    metadata,
                    state,
                    state_size,
                    files: &mut self.file_io,
                    pending: &self.pending_host_events,
                    read_seq: &self.runtime_event_read_seq,
                    error: None,
                    calls: 0,
                    return_budget: self.native_host_return_budget,
                    round_budget: self.native_host_round_budget,
                    nbas: &mut self.native_nba_queue,
                    delayed: &mut self.native_delayed_nbas,
                    pending_scan: 0,
                    comb_before: Vec::new(),
                    observer: None,
                    updates: self.observes_updates,
                };
                let ((result, error), elapsed) = match prepared {
                    Some(call) => {
                        scope.observer = Some(&mut observer);
                        scope.run_prepared(call)
                    }
                    None => (scope.run(observer.backend, index), None),
                };
                self.native_service_calls = self.native_service_calls.saturating_add(scope.calls);
                observer.backend.complete_process(elapsed);
                error.map_or(result, Err)
            } else {
                self.backend.run_process(index)
            }
        } else {
            self.backend.run_process(index)
        };
        if native_observers {
            if self.native_arena.header.observer_evaluating != 0 {
                // A finish or fault can leave the generated settle early.
                // The reference captures its final sensitivity even then.
                self.comb_observer_snapshots = self.snapshot_all_comb_observers();
                self.backend.set_comb_capture_event_enabled(&vec![
                    false;
                    self.program
                        .runtime_schema
                        .runtime_event_sites
                        .len()
                ]);
            } else {
                self.comb_observer_reads.unpack(
                    &self.native_arena.observers,
                    &mut self.comb_observer_snapshots,
                );
            }
            self.comb_observer_initial_eval = self.native_arena.header.observer_initial != 0;
        }
        self.checked_kernel_result(result, start)
    }

    pub(crate) fn eval_apply_ff_at_checked(
        &mut self,
        event: B::Event,
    ) -> Result<(), RuntimeErrorCode> {
        let start = self.runtime_event_write_seq();
        let result = self.backend.eval_apply_ff_at(event);
        self.checked_kernel_result(result, start)
    }

    pub(crate) fn eval_comb_apply_ff_at_checked(
        &mut self,
        event: B::Event,
    ) -> Result<(), RuntimeErrorCode> {
        let start = self.runtime_event_write_seq();
        let result = self.backend.eval_comb_apply_ff_at(event);
        self.checked_kernel_result(result, start)
    }

    pub(crate) fn eval_only_ff_at_checked(
        &mut self,
        event: B::Event,
    ) -> Result<(), RuntimeErrorCode> {
        let start = self.runtime_event_write_seq();
        let result = self.backend.eval_only_ff_at(event);
        self.checked_kernel_result(result, start)
    }

    pub(crate) fn apply_ff_at_checked(&mut self, event: B::Event) -> Result<(), RuntimeErrorCode> {
        let start = self.runtime_event_write_seq();
        let result = self.backend.apply_ff_at(event);
        self.checked_kernel_result(result, start)
    }

    /// Directly execute combinational logic evaluation.
    pub fn eval_comb(&mut self) -> Result<(), RuntimeErrorCode> {
        self.eval_comb_checked()?;
        self.dirty = false;
        self.end_untimed_slot()
    }

    /// The work a partitioned settle counted since the simulation
    /// started, when built with `settle_profile`.
    pub fn settle_profile(&self) -> Option<celox_state_layout::SettleProfile> {
        let settle = self.layout().settle.as_ref()?;
        let (memory, size) = self.memory_as_ptr();
        // SAFETY: the backend's state image is `size` readable bytes and
        // nothing writes it while `self` is borrowed.
        let memory = unsafe { std::slice::from_raw_parts(memory, size) };
        settle.profile(memory)
    }
}
