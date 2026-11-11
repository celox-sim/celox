//! The simulation backend interface of the native backend.

use super::*;

impl super::super::super::SimBackend for NativeBackend {
    type Event = NativeEventRef;

    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        if self.lane_comb_kernel() {
            let compiled = Arc::clone(&self.compiled);
            let kernels = compiled
                .lane_kernels
                .as_ref()
                .expect("checked lane kernels");
            let kernel = kernels.comb.as_ref().expect("checked comb lane kernel");
            let selection = compiled.options.kernel_selection;
            return super::super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::super::lanes::LaneKernelSlot::Comb,
                selection,
                |backend| backend.run_lane_kernel(kernels.lanes, kernel),
                |backend| backend.call_func_timed(compiled.comb_func),
            );
        }
        let func = self.compiled.comb_func;
        self.call_func_timed(func)
    }

    fn eval_clock_aliases(&mut self, event: NativeEventRef) -> Result<bool, SimulatorErrorCode> {
        let Some(func) = self.compiled.clock_alias_funcs[event.id] else {
            return Ok(false);
        };
        self.call_func_timed(func)?;
        Ok(true)
    }

    fn eval_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        if self.lane_event_kernel(event) {
            let compiled = Arc::clone(&self.compiled);
            let kernels = compiled
                .lane_kernels
                .as_ref()
                .expect("checked lane kernels");
            let kernel = kernels.events[event.id]
                .as_ref()
                .expect("checked event lane kernel");
            let selection = compiled.options.kernel_selection;
            return super::super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::super::lanes::LaneKernelSlot::Event(event.id),
                selection,
                |backend| backend.run_lane_kernel(kernels.lanes, kernel),
                |backend| backend.call_func_timed(event.func),
            );
        }
        self.call_func_timed(event.func)
    }

    fn eval_comb_apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        let Some(comb_apply_func) = event.comb_apply_func else {
            self.eval_comb()?;
            return self.eval_apply_ff_at(event);
        };
        // Partitioned phases compete with the fused sequential function,
        // which shares state between its combinational and FF parts.
        if self.lane_fused_kernel(event) {
            let compiled = Arc::clone(&self.compiled);
            let kernels = compiled
                .lane_kernels
                .as_ref()
                .expect("checked lane kernels");
            let kernel = kernels.fused[event.id]
                .as_ref()
                .expect("checked fused lane kernel");
            let selection = compiled.options.kernel_selection;
            return super::super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::super::lanes::LaneKernelSlot::Fused(event.id),
                selection,
                |backend| backend.run_lane_kernel(kernels.lanes, kernel),
                |backend| backend.call_func_timed(comb_apply_func),
            );
        }
        if self.lane_comb_kernel() || self.lane_event_kernel(event) {
            let selection = self.compiled.options.kernel_selection;
            return super::super::super::lanes::run_selected(
                self,
                |backend| &mut *backend.lane_selectors,
                super::super::super::lanes::LaneKernelSlot::Fused(event.id),
                selection,
                |backend| {
                    backend.eval_comb()?;
                    backend.eval_apply_ff_at(event)
                },
                |backend| backend.call_func_timed(comb_apply_func),
            );
        }
        self.call_func_timed(comb_apply_func)
    }

    fn eval_comb_apply_ff_many_at(
        &mut self,
        event: NativeEventRef,
        count: u64,
    ) -> (u64, Result<(), SimulatorErrorCode>) {
        let fused_sequential = self
            .lane_selectors
            .decided(super::super::super::lanes::LaneKernelSlot::Fused(event.id))
            == Some(celox_runtime::parallel::KernelChoice::Sequential);
        let comb_apply_func = match event.comb_apply_func {
            Some(func)
                if !(self.lane_comb_kernel() || self.lane_event_kernel(event))
                    || fused_sequential =>
            {
                func
            }
            _ => {
                if count == 0 {
                    return (0, Ok(()));
                }
                return (1, self.eval_comb_apply_ff_at(event));
            }
        };
        if self.compiled.options.native_tick_loop {
            let (executed, result) = self.call_func_many_timed(comb_apply_func, count);
            if fused_sequential {
                self.lane_selectors.advance(
                    super::super::super::lanes::LaneKernelSlot::Fused(event.id),
                    executed,
                );
            }
            (executed, result)
        } else if count == 0 {
            (0, Ok(()))
        } else {
            if fused_sequential {
                self.lane_selectors.advance(
                    super::super::super::lanes::LaneKernelSlot::Fused(event.id),
                    1,
                );
            }
            (1, self.call_func_timed(comb_apply_func))
        }
    }

    fn eval_only_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
    }

    fn apply_ff_at(&mut self, event: NativeEventRef) -> Result<(), SimulatorErrorCode> {
        self.call_func_timed(event.func)
    }

    fn run_process(&mut self, index: usize) -> Result<(), SimulatorErrorCode> {
        let func = *self
            .compiled
            .process_funcs
            .get(index)
            .ok_or(SimulatorErrorCode::InternalError)?;
        self.call_func_timed(func)
    }

    fn prepare_process(&mut self, index: usize) -> Option<celox_runtime::backend::PreparedProcess> {
        self.compiled.process_funcs.get(index)?;
        Some(celox_runtime::backend::PreparedProcess::new(
            self.compiled.clone(),
            self.memory.owner(),
            index,
            self.memory.as_mut_ptr().cast(),
            self.memory.len_words() * 8,
            self.execution_timing.is_some(),
        ))
    }

    fn complete_process(&mut self, elapsed: Option<std::time::Duration>) {
        if let (Some(elapsed), Some(timing)) = (elapsed, &mut self.execution_timing) {
            timing.elapsed = timing.elapsed.saturating_add(elapsed);
            timing.calls = timing.calls.saturating_add(1);
        }
    }

    fn resolve_signal(&self, addr: &AbsoluteAddr) -> SignalRef {
        let layout = &self.compiled.layout;
        let offset = layout.offsets.get(addr).copied().unwrap_or(0);
        let width = layout.widths.get(addr).copied().unwrap_or(0);
        let is_4state = layout.is_4states.get(addr).copied().unwrap_or(false);
        let array_layout = layout
            .unpacked_arrays
            .get(addr)
            .map(|array| SignalArrayLayout {
                element_width: array.element_width,
                element_count: array.element_count,
                element_stride: array.element_stride,
                plane_size: array.plane_size,
            });
        SignalRef {
            offset,
            width,
            is_4state,
            array_layout,
        }
    }

    fn resolve_event(&self, addr: &AbsoluteAddr) -> NativeEventRef {
        *self
            .compiled
            .event_map
            .get(addr)
            .unwrap_or_else(|| panic!("event not found for {:?}", addr))
    }

    fn resolve_event_opt(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.event_map.get(addr).copied()
    }

    fn resolve_eval_only_event(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.eval_only_event_map.get(addr).copied()
    }

    fn resolve_apply_event(&self, addr: &AbsoluteAddr) -> Option<NativeEventRef> {
        self.compiled.apply_event_map.get(addr).copied()
    }

    fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        celox_runtime::backend::SimBackend::mark_vcd_signal(self, signal);
        self.store_scalar(signal, val);
    }

    #[inline]
    fn set_scheduler_slot<T: Copy>(&mut self, signal: SignalRef, val: T) {
        if Self::exact_scalar::<T>(signal)
            && !(self.compiled.options.four_state && signal.is_4state)
        {
            // SAFETY: the slot is a scalar of exactly `T`'s bytes inside the
            // state image, without a mask plane to clear.
            unsafe {
                (self.memory.as_mut_ptr() as *mut u8)
                    .add(signal.offset)
                    .cast::<T>()
                    .write_unaligned(val);
            }
        } else {
            self.store_scalar(signal, val);
        }
    }

    fn set_wide(&mut self, signal: SignalRef, val: BigUint) {
        celox_runtime::backend::SimBackend::mark_vcd_signal(self, signal);
        let clear_mask = self.compiled.options.four_state && signal.is_4state;
        self.write_signal_plane(signal, false, &val);
        if clear_mask {
            self.write_signal_plane(signal, true, &BigUint::from(0u8));
        }
    }

    fn set_four_state(&mut self, signal: SignalRef, val: BigUint, mask: BigUint) {
        celox_runtime::backend::SimBackend::mark_vcd_signal(self, signal);
        let write_mask = self.compiled.options.four_state && signal.is_4state;
        self.write_signal_plane(signal, false, &val);
        if write_mask {
            self.write_signal_plane(signal, true, &mask);
        }
    }

    fn get(&self, signal: SignalRef) -> BigUint {
        self.read_signal_plane(signal, false)
    }

    #[inline]
    fn get_scheduler_slot<T: Default + Copy>(&self, signal: SignalRef) -> T {
        if Self::exact_scalar::<T>(signal) {
            // SAFETY: the slot is a scalar of exactly `T`'s bytes inside the
            // state image.
            unsafe {
                (self.memory.as_ptr() as *const u8)
                    .add(signal.offset)
                    .cast::<T>()
                    .read_unaligned()
            }
        } else {
            self.get_as(signal)
        }
    }

    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        let bs = get_byte_size(signal.width);
        let provided_size = std::mem::size_of::<T>();
        if signal.array_layout.is_some() {
            let mut val = T::default();
            let value = self.read_signal_plane(signal, false).to_bytes_le();
            let val_bytes = unsafe {
                std::slice::from_raw_parts_mut(&mut val as *mut T as *mut u8, provided_size)
            };
            let copy_len = value.len().min(val_bytes.len());
            val_bytes[..copy_len].copy_from_slice(&value[..copy_len]);
            return val;
        }
        let ptr = unsafe { (self.memory.as_ptr() as *const u8).add(signal.offset) };
        if provided_size <= bs {
            return unsafe { std::ptr::read_unaligned(ptr as *const T) };
        }

        let bytes = self.mem_bytes();
        let mut val = T::default();
        let val_bytes =
            unsafe { std::slice::from_raw_parts_mut(&mut val as *mut T as *mut u8, provided_size) };
        let copy_len = val_bytes.len().min(bs);
        val_bytes[..copy_len].copy_from_slice(&bytes[signal.offset..signal.offset + copy_len]);
        val
    }

    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let val = self.read_signal_plane(signal, false);
        let mask = if self.compiled.options.four_state && signal.is_4state {
            self.read_signal_plane(signal, true)
        } else {
            BigUint::from(0u32)
        };
        (val, mask)
    }

    fn memory_as_ptr(&self) -> (*const u8, usize) {
        (self.mem_ptr(), self.memory.len_words() * 8)
    }

    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        let view = (
            self.memory.expose_mut_ptr().cast(),
            self.memory.len_words() * 8,
        );
        self.mark_settle_untracked();
        view
    }

    fn memory_owner(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.memory.owner())
    }

    fn write_memory(&mut self, offset: usize, bytes: &[u8]) {
        self.memory.write_bytes(offset, bytes);
        self.mark_settle_range(offset, bytes.len());
        if !self.memory.vcd_tracking_enabled() {
            self.mark_settle_untracked();
        }
    }

    fn vcd_tracking_enabled(&self) -> bool {
        self.memory.vcd_tracking_enabled()
    }

    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        (
            self.runtime_event_buffer.as_ptr(),
            self.runtime_event_buffer.byte_size(),
        )
    }

    fn runtime_event_buffer(&self) -> Option<Arc<RuntimeEventBuffer>> {
        Some(Arc::clone(&self.runtime_event_buffer))
    }

    fn set_comb_capture_event_enabled(&mut self, active_sites: &[bool]) {
        self.comb_capture_enabled.fill(0);
        for (idx, active) in active_sites.iter().copied().enumerate() {
            if active && idx < self.comb_capture_enabled.len() {
                self.comb_capture_enabled[idx] = 1;
            }
        }
    }

    fn comb_capture_event_enabled(&self) -> Vec<bool> {
        self.comb_capture_enabled
            .iter()
            .take(self.compiled.layout.runtime_event_site_layouts.len())
            .map(|&enabled| enabled != 0)
            .collect()
    }

    fn stable_region_size(&self) -> usize {
        self.compiled.layout.total_size
    }

    fn layout(&self) -> &MemoryLayout {
        &self.compiled.layout
    }

    fn heap(&self) -> &celox_heap::Heap {
        &self.heap
    }

    fn heap_mut(&mut self) -> &mut celox_heap::Heap {
        &mut self.heap
    }

    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        &self.compiled.id_to_addr
    }

    fn id_to_event_slice(&self) -> &[NativeEventRef] {
        &self.compiled.id_to_event
    }

    fn num_events(&self) -> usize {
        self.compiled.id_to_event.len()
    }

    fn clear_triggered_bits(&mut self) {
        let offset = self.compiled.layout.triggered_bits_offset;
        let size = self.compiled.layout.triggered_bits_total_size;
        let bytes = self.mem_bytes_mut();
        bytes[offset..offset + size].fill(0);
    }

    fn mark_triggered_bit(&mut self, id: usize) {
        let offset = self.compiled.layout.triggered_bits_offset;
        let byte_idx = offset + id / 8;
        let bit_idx = id % 8;
        self.mem_bytes_mut()[byte_idx] |= 1 << bit_idx;
    }

    fn get_triggered_bits(&self) -> BitSet {
        let offset = self.compiled.layout.triggered_bits_offset;
        let size = self.compiled.layout.triggered_bits_total_size;
        celox_runtime::backend::triggered_bits_from_bytes(&self.mem_bytes()[offset..offset + size])
    }
}
