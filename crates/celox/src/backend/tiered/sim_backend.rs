//! The simulation backend interface of the tiered backend.

use super::*;

impl SimBackend for TieredBackend {
    /// Only generated code runs the scheduler unit faster than the
    /// runtime's loop.
    fn runs_compiled_scheduler(&self) -> bool {
        match &self.phase {
            Phase::Compiled(_) => true,
            Phase::Interpreting(Some(interp)) => interp.runs_compiled_scheduler(),
            _ => false,
        }
    }

    fn compiled_scheduler_budget(&self) -> u64 {
        // A native loop or hotspot scheduler must return periodically so
        // a pending whole-image promotion or an optimizing upgrade can be
        // adopted while a long simulation is still running.
        if self.upgrade_receiver.is_some() || matches!(self.promotion, Promotion::Pending(_)) {
            1024
        } else {
            u64::MAX
        }
    }

    fn runs_derived_edge_units(&self) -> bool {
        matches!(self.phase, Phase::Compiled(_)) && self.derived_edges_live
    }

    fn request_derived_edge_units(&mut self) {
        if let Some(request) = &mut self.derived_compile_request {
            request.request();
        }
    }

    type Event = TieredEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        self.maybe_upgrade();
        self.count_evaluation_for_current_tier();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_comb(),
            Phase::Compiled(compiled) => compiled.eval_comb(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_clock_aliases(&mut self, event: TieredEventRef) -> Result<bool, SimulatorErrorCode> {
        // Do not promote during a partial settle: normal evaluation boundaries
        // continue to own tier changes and their accounting.
        match &mut self.phase {
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                let event = native.id_to_event_slice()[event.id()];
                native.eval_clock_aliases(event)
            }
            _ => Ok(false),
        }
    }

    fn eval_apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        self.maybe_upgrade();
        self.count_evaluation_for_current_tier();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_apply_ff_at(
                super::super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.eval_apply_ff_at(event.id()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_comb_apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        self.maybe_upgrade();
        self.count_evaluation_for_current_tier();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_comb_apply_ff_at(
                super::super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.eval_comb_apply_ff_at(event.id()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_comb_apply_ff_many_at(
        &mut self,
        event: TieredEventRef,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        // Poll promotion once for the whole batch so an adopted tier runs
        // every remaining iteration inside generated code.
        if count == 0 {
            return (0, Ok(()));
        }
        self.maybe_promote();
        self.maybe_upgrade();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => {
                self.interpreted_steps = self.interpreted_steps.saturating_add(1);
                (
                    1,
                    interp.eval_comb_apply_ff_at(super::super::interp::InterpEventRef::from_parts(
                        event.addr(),
                        event.id(),
                    )),
                )
            }
            Phase::Compiled(compiled) => {
                let (completed, result) = compiled.eval_comb_apply_ff_many_at(event.id(), count);
                self.compiled_steps = self.compiled_steps.saturating_add(completed);
                (completed, result)
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn eval_only_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        self.maybe_upgrade();
        self.count_evaluation_for_current_tier();
        let result = match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.eval_only_ff_at(
                super::super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.eval_only_ff_at_addr(&event.addr()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        };
        // The paired apply phase must run on the same tier; defer promotion
        // until it completes.
        self.pending_split_applies = self.pending_split_applies.saturating_add(1);
        result
    }

    fn apply_ff_at(&mut self, event: TieredEventRef) -> Result<(), SimulatorErrorCode> {
        // Never promote between the evaluate-only and apply phases.
        self.maybe_promote();
        self.maybe_upgrade();
        self.count_evaluation_for_current_tier();
        let result = match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.apply_ff_at(
                super::super::interp::InterpEventRef::from_parts(event.addr(), event.id()),
            ),
            Phase::Compiled(compiled) => compiled.apply_ff_at_addr(&event.addr()),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        };
        self.pending_split_applies = self.pending_split_applies.saturating_sub(1);
        result
    }

    fn prepare_process(&mut self, index: usize) -> Option<celox_runtime::backend::PreparedProcess> {
        self.maybe_promote();
        self.maybe_upgrade();
        let call = match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.prepare_process(index),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.prepare_process(index),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.prepare_process(index),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        };
        if call.is_some() {
            self.count_evaluation_for_current_tier();
        }
        call
    }

    fn complete_process(&mut self, elapsed: Option<std::time::Duration>) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.complete_process(elapsed),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.complete_process(elapsed),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.complete_process(elapsed),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn run_process(&mut self, index: usize) -> Result<(), SimulatorErrorCode> {
        self.maybe_promote();
        self.maybe_upgrade();
        self.count_evaluation_for_current_tier();
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.run_process(index),
            Phase::Compiled(compiled) => compiled.run_process(index),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.resolve_signal(addr),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.resolve_signal(addr),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.resolve_signal(addr),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> TieredEventRef {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                let ev = interp.resolve_event(addr);
                TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }
            }
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                let ev = jit.resolve_event(addr);
                TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                let ev = native.resolve_event(addr);
                TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<TieredEventRef> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.resolve_event_opt(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.resolve_event_opt(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.resolve_event_opt(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<TieredEventRef> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp
                    .resolve_eval_only_event(addr)
                    .map(|ev| TieredEventRef {
                        addr: ev.addr(),
                        id: ev.id(),
                    })
            }
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.resolve_eval_only_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native
                .resolve_eval_only_event(addr)
                .map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                }),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<TieredEventRef> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.resolve_apply_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.resolve_apply_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.resolve_apply_event(addr).map(|ev| TieredEventRef {
                    addr: ev.addr(),
                    id: ev.id(),
                })
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, value: T) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set(signal, value),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set(signal, value),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.set(signal, value),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_scheduler_slot<T: Copy>(&mut self, signal: SignalRef, value: T) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_scheduler_slot(signal, value),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set_scheduler_slot(signal, value),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.set_scheduler_slot(signal, value)
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_wide(signal, value),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set_wide(signal, value),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.set_wide(signal, value),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.set_four_state(signal, value, mask),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.set_four_state(signal, value, mask),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.set_four_state(signal, value, mask)
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get(signal),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_as(signal),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_as(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_as(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_scheduler_slot<T: Default + Copy>(&self, signal: SignalRef) -> T {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_scheduler_slot(signal),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_scheduler_slot(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_scheduler_slot(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_four_state(signal),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_four_state(signal),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_four_state(signal),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_as_ptr(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.memory_as_ptr(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.memory_as_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_as_mut_ptr(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.memory_as_mut_ptr(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.memory_as_mut_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.memory_owner(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.memory_owner(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.memory_owner(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn save_call_stack(&self, len: usize, into: &mut Vec<u8>) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.save_call_stack(len, into),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.save_call_stack(len, into),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.save_call_stack(len, into),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn restore_call_stack(&mut self, bytes: &[u8]) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.restore_call_stack(bytes),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.restore_call_stack(bytes),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.restore_call_stack(bytes),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn write_memory(&mut self, offset: usize, bytes: &[u8]) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.write_memory(offset, bytes),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.write_memory(offset, bytes),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.write_memory(offset, bytes),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn vcd_tracking_enabled(&self) -> bool {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.vcd_tracking_enabled(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.vcd_tracking_enabled(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.vcd_tracking_enabled(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.runtime_event_buffer_as_ptr(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.runtime_event_buffer_as_ptr(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.runtime_event_buffer_as_ptr(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.runtime_event_buffer(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => Some(jit.runtime_event_buffer().clone()),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.runtime_event_buffer(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => {
                interp.set_comb_capture_event_enabled(active_sites)
            }
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => {
                jit.set_comb_capture_event_enabled(active_sites)
            }
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => {
                native.set_comb_capture_event_enabled(active_sites)
            }
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn comb_capture_event_enabled(&self) -> Vec<bool> {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.comb_capture_event_enabled(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.comb_capture_event_enabled(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.comb_capture_event_enabled(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn stable_region_size(&self) -> usize {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.stable_region_size(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.stable_region_size(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.stable_region_size(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn layout(&self) -> &MemoryLayout {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.layout(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.layout(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.layout(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn heap(&self) -> &celox_heap::Heap {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.heap(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.heap(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.heap(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn heap_mut(&mut self) -> &mut celox_heap::Heap {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.heap_mut(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.heap_mut(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.heap_mut(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.id_to_addr_slice(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.id_to_addr_slice(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.id_to_addr_slice(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn id_to_event_slice(&self) -> &[TieredEventRef] {
        &self.events
    }

    fn num_events(&self) -> usize {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.num_events(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.num_events(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.num_events(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn clear_triggered_bits(&mut self) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.clear_triggered_bits(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.clear_triggered_bits(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.clear_triggered_bits(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        match &mut self.phase {
            Phase::Interpreting(Some(interp)) => interp.mark_triggered_bit(id),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.mark_triggered_bit(id),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.mark_triggered_bit(id),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }

    fn get_triggered_bits(&self) -> bit_set::BitSet {
        match &self.phase {
            Phase::Interpreting(Some(interp)) => interp.get_triggered_bits(),
            #[cfg(feature = "cranelift")]
            Phase::Compiled(CompiledTier::Jit(jit)) => jit.get_triggered_bits(),
            #[cfg(any(
                target_arch = "x86_64",
                feature = "arm64-codegen",
                target_arch = "aarch64"
            ))]
            Phase::Compiled(CompiledTier::Native(native)) => native.get_triggered_bits(),
            Phase::Interpreting(None) => unreachable!("promoted backend left no interpreter"),
        }
    }
}
