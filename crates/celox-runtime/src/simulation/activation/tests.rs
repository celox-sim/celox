use super::*;
use crate::{AbsoluteAddr, MemoryLayout, backend::EventHandle};
use num_bigint::BigUint;

#[derive(Clone, Copy, Debug)]
struct NoEvent;
impl EventHandle for NoEvent {
    fn id(&self) -> usize {
        unreachable!()
    }
    fn addr(&self) -> AbsoluteAddr {
        unreachable!()
    }
}

/// A deliberately destructive kernel: even a failing call overwrites all
/// rebound slots. This exercises cleanup after backend errors and after
/// an unexpected terminal status without relying on a particular JIT.
struct ProbeBackend {
    values: Vec<Value>,
    outcome: Option<celox_design::ProcessStatus>,
}
impl SimBackend for ProbeBackend {
    type Event = NoEvent;
    fn run_process(&mut self, kernel: usize) -> Result<(), SimulatorErrorCode> {
        assert_eq!(kernel, 9);
        assert_eq!(self.values[0].0, 12u8.into());
        assert_eq!(self.values[3].0, 103u8.into());
        assert_eq!(self.values[4], (3u8.into(), 2u8.into()));
        for value in &mut self.values {
            *value = (255u8.into(), 255u8.into());
        }
        match self.outcome {
            Some(status) => {
                self.values[1] = (status.code().into(), 0u8.into());
                self.values[5] = (42u8.into(), 4u8.into());
                Ok(())
            }
            None => Err(SimulatorErrorCode::InternalError),
        }
    }
    fn set<T: Copy>(&mut self, signal: SignalRef, val: T) {
        assert!(std::mem::size_of::<T>() <= 8);
        let mut bytes = [0u8; 8];
        // SAFETY: val is readable for size_of::<T>() bytes; the array is
        // eight bytes and the assertion bounds the copy.
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&val as *const T).cast::<u8>(),
                bytes.as_mut_ptr(),
                std::mem::size_of::<T>(),
            );
        }
        self.values[signal.offset] = (u64::from_ne_bytes(bytes).into(), 0u8.into());
    }
    fn get_as<T: Default + Copy>(&self, signal: SignalRef) -> T {
        assert!(std::mem::size_of::<T>() <= 8);
        let bytes = self.values[signal.offset]
            .0
            .to_u64_digits()
            .first()
            .copied()
            .unwrap_or(0)
            .to_ne_bytes();
        // SAFETY: the probe requests only integer status/control types,
        // for which all bit patterns are valid, within the checked width.
        unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<T>()) }
    }
    fn set_wide(&mut self, signal: SignalRef, value: BigUint) {
        self.values[signal.offset] = (value, 0u8.into());
    }
    fn set_four_state(&mut self, signal: SignalRef, value: BigUint, mask: BigUint) {
        self.values[signal.offset] = (value, mask);
    }
    fn get(&self, signal: SignalRef) -> BigUint {
        self.values[signal.offset].0.clone()
    }
    fn get_four_state(&self, signal: SignalRef) -> (BigUint, BigUint) {
        let value = &self.values[signal.offset];
        (value.0.clone(), value.1.clone())
    }
    fn eval_comb(&mut self) -> Result<(), SimulatorErrorCode> {
        unreachable!()
    }
    fn eval_apply_ff_at(&mut self, _: NoEvent) -> Result<(), SimulatorErrorCode> {
        unreachable!()
    }
    fn eval_only_ff_at(&mut self, _: NoEvent) -> Result<(), SimulatorErrorCode> {
        unreachable!()
    }
    fn apply_ff_at(&mut self, _: NoEvent) -> Result<(), SimulatorErrorCode> {
        unreachable!()
    }
    fn resolve_signal(&self, _: &AbsoluteAddr) -> SignalRef {
        unreachable!()
    }
    fn resolve_event(&self, _: &AbsoluteAddr) -> NoEvent {
        unreachable!()
    }
    fn resolve_event_opt(&self, _: &AbsoluteAddr) -> Option<NoEvent> {
        unreachable!()
    }
    fn resolve_eval_only_event(&self, _: &AbsoluteAddr) -> Option<NoEvent> {
        unreachable!()
    }
    fn resolve_apply_event(&self, _: &AbsoluteAddr) -> Option<NoEvent> {
        unreachable!()
    }
    fn memory_as_ptr(&self) -> (*const u8, usize) {
        unreachable!()
    }
    fn memory_as_mut_ptr(&mut self) -> (*mut u8, usize) {
        unreachable!()
    }
    fn runtime_event_buffer_as_ptr(&self) -> (*const u8, usize) {
        unreachable!()
    }
    fn stable_region_size(&self) -> usize {
        unreachable!()
    }
    fn layout(&self) -> &MemoryLayout {
        unreachable!()
    }
    fn heap(&self) -> &celox_heap::Heap {
        unreachable!()
    }
    fn heap_mut(&mut self) -> &mut celox_heap::Heap {
        unreachable!()
    }
    fn id_to_addr_slice(&self) -> &[AbsoluteAddr] {
        unreachable!()
    }
    fn id_to_event_slice(&self) -> &[NoEvent] {
        unreachable!()
    }
    fn num_events(&self) -> usize {
        unreachable!()
    }
    fn clear_triggered_bits(&mut self) {
        unreachable!()
    }
    fn mark_triggered_bit(&mut self, _: usize) {
        unreachable!()
    }
    fn get_triggered_bits(&self) -> bit_set::BitSet {
        unreachable!()
    }
}

#[test]
fn probe_restores_all_slots_on_success_backend_error_and_unexpected_status() {
    let slot = |offset| SignalRef {
        offset,
        width: 8,
        is_4state: true,
        array_layout: None,
    };
    let private: Vec<_> = [0, 1, 2, 3, 5].into_iter().map(slot).collect();
    let probe = celox_design::ProcessEventProbe {
        result: slot(5),
        resume: 12,
        inputs: vec![slot(4)],
        scratch: vec![slot(5), slot(6)],
    };
    let mut refs = crate::timing::ProcessRefs::new(slot(1), slot(2));
    refs.resume = slot(0);
    refs.frame = Some(celox_design::ProcessFrame {
        resume_count: 13,
        private: private.clone(),
        locals: vec![slot(4)],
        random: slot(3),
        seed: 1,
        starts_with_event_wait: false,
        event_probes: vec![probe.clone()],
    });
    let mut activation = Activation::root(9);
    activation.private = private
        .iter()
        .map(|slot| ((100u64 + slot.offset as u64).into(), 0u8.into()))
        .collect();
    activation.locals = vec![Some(0)];
    let cells = vec![(3u8.into(), 2u8.into())];
    let activation_before = activation.clone();
    for outcome in [
        Some(celox_design::ProcessStatus::EventProbe),
        None,
        Some(celox_design::ProcessStatus::Yield),
    ] {
        let before: Vec<_> = (0u8..7)
            .map(|value| (value.into(), (value + 8).into()))
            .collect();
        let mut backend = ProbeBackend {
            values: before.clone(),
            outcome,
        };
        let result = event_probe(&mut backend, &refs, &activation, &cells, &probe);
        if outcome == Some(celox_design::ProcessStatus::EventProbe) {
            assert_eq!(result.unwrap(), (42u8.into(), 4u8.into()));
        } else {
            assert!(result.is_err());
        }
        assert_eq!(backend.values, before);
        assert_eq!(activation, activation_before);
        assert_eq!(cells, [(3u8.into(), 2u8.into())]);
    }
}
