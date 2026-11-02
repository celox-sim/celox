//! Incremental compilation at complete interpreter invocation boundaries.
//!
//! The simulation thread alone installs and calls entries. The worker owns no
//! live state. Bounded jobs select hot groups and unconditional block regions;
//! cold or unsupported work remains interpreted.

use std::{
    collections::BTreeMap,
    sync::{Arc, mpsc},
    thread::JoinHandle,
    time::{Duration, Instant},
};
mod region;
pub(crate) use region::RegionSource;
use region::{BlockCode, BlockRequest};

use super::{SimulatorErrorCode, compile_cancel::CompileCancel, native::backend::HotspotCode};
use crate::{
    SimulatorOptions,
    ir::{ExecutionUnit, RegionedAbsoluteAddr},
};

pub(crate) const SCRATCH_BYTES: usize = 1024 * 1024;
const MAX_INSTRUCTIONS: usize = 32_768;
const MAX_LARGE_INSTRUCTIONS: usize = 1_048_576;
const LARGE_HOT_COST: Duration = Duration::from_millis(500);
const MAX_CODE_BYTES: usize = 16 * 1024 * 1024;
const HOT_COST: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EntryKey {
    Comb,
    CombApply(usize),
    EvalApply(usize),
    EvalOnly(usize),
    Apply(usize),
    Process(usize),
    Block(u8, usize, usize, celox_sir::BlockId),
}

impl EntryKey {
    fn block(self, unit: usize, block: celox_sir::BlockId) -> Self {
        let (kind, event) = match self {
            Self::Comb => (0, 0),
            Self::CombApply(id) => (1, id),
            Self::EvalApply(id) => (2, id),
            Self::EvalOnly(id) => (3, id),
            Self::Apply(id) => (4, id),
            Self::Process(id) => (5, id),
            Self::Block(..) => unreachable!("block keys do not contain groups"),
        };
        Self::Block(kind, event, unit, block)
    }
}

/// Progress of experimental incremental compilation, separate from whole-image
/// promotion: compiled calls can coexist with interpreted calls permanently.
#[derive(Debug, Clone, Default)]
pub struct HotspotStats {
    pub interpreted_calls: u64,
    pub compiled_calls: u64,
    pub compiled_entries: usize,
    pub rejected_entries: usize,
    pub failed_entries: usize,
    pub code_bytes: usize,
    pub compile_time: Duration,
    pub compiled_block_calls: u64,
    pub frame_bytes_transferred: u64,
}

#[derive(Default)]
struct Entry {
    calls: u64,
    estimated_cost: Duration,
    unavailable: bool,
    block_regions: bool,
    large_attempted: bool,
    code: Option<EntryCode>,
}

enum EntryCode {
    Group(Arc<HotspotCode>),
    Block(BlockCode),
}
impl EntryCode {
    fn native(&self) -> &HotspotCode {
        match self {
            Self::Group(code) => code,
            Self::Block(code) => &code.code,
        }
    }
}

struct Request {
    key: EntryKey,
    units: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    block: Option<BlockRequest>,
}

struct Completed {
    key: EntryKey,
    code: Result<EntryCode, String>,
    elapsed: Duration,
}

pub(crate) struct HotspotRuntime {
    entries: BTreeMap<EntryKey, Entry>,
    requests: Option<mpsc::SyncSender<Request>>,
    completed: mpsc::Receiver<Completed>,
    worker: Option<JoinHandle<()>>,
    cancel: CompileCancel,
    busy: bool,
    polls: u64,
    stats: HotspotStats,
}

impl HotspotRuntime {
    pub(crate) fn new(
        layout: &crate::backend::MemoryLayout,
        options: &SimulatorOptions,
    ) -> std::io::Result<Self> {
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        let (sender, completed) = mpsc::channel();
        let cancel = CompileCancel::new();
        let worker_cancel = cancel.clone();
        let layout = layout.clone();
        let options = options.clone();
        let worker = std::thread::Builder::new()
            .name("celox-jit-hotspot".into())
            .spawn(move || {
                while let Ok(request) = receiver.recv() {
                    if worker_cancel.is_cancelled() {
                        break;
                    }
                    let start = Instant::now();
                    let code = match request.block {
                        Some(block) => block
                            .compile(&layout, &options, &worker_cancel)
                            .map(EntryCode::Block),
                        None => {
                            HotspotCode::compile(&request.units, &layout, &options, &worker_cancel)
                                .map(Arc::new)
                                .map(EntryCode::Group)
                        }
                    }
                    .map_err(|error| error.to_string());
                    if worker_cancel.is_cancelled() {
                        break;
                    }
                    if sender
                        .send(Completed {
                            key: request.key,
                            code,
                            elapsed: start.elapsed(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            entries: BTreeMap::new(),
            requests: Some(requests),
            completed,
            worker: Some(worker),
            cancel,
            busy: false,
            polls: 0,
            stats: HotspotStats::default(),
        })
    }

    pub(crate) fn stats(&self) -> HotspotStats {
        self.stats.clone()
    }

    pub(crate) fn cancel(&mut self) -> bool {
        let active = !self.cancel.is_cancelled();
        self.cancel.cancel();
        self.requests.take();
        self.busy = false;
        active
    }

    /// This point is between complete invocations; no native activation can
    /// retain a pointer to an entry being installed here. Dirty metadata is
    /// intentionally preserved, including across split evaluate/apply calls.
    pub(crate) fn try_run(
        &mut self,
        key: EntryKey,
        memory: &mut [u64],
    ) -> Option<Result<(), SimulatorErrorCode>> {
        self.polls = self.polls.wrapping_add(1);
        if self.busy && self.polls.is_multiple_of(64) {
            match self.completed.try_recv() {
                Ok(completed) => {
                    self.busy = false;
                    self.stats.compile_time += completed.elapsed;
                    let entry = self.entries.entry(completed.key).or_default();
                    match completed.code {
                        Ok(code)
                            if code.native().required_state_size <= memory.len() * 8
                                && self
                                    .stats
                                    .code_bytes
                                    .saturating_add(code.native().code_size)
                                    <= MAX_CODE_BYTES =>
                        {
                            tracing::debug!(key = ?completed.key, elapsed = ?completed.elapsed,
                                bytes = code.native().code_size, "hotspot entry published");
                            self.stats.code_bytes += code.native().code_size;
                            self.stats.compiled_entries += 1;
                            entry.code = Some(code);
                        }
                        Ok(_) => {
                            entry.unavailable = true;
                            self.stats.rejected_entries += 1;
                        }
                        Err(error) => {
                            tracing::debug!(key = ?completed.key, error, "hotspot compilation failed");
                            entry.unavailable = true;
                            self.stats.failed_entries += 1;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.busy = false;
                    self.cancel();
                    self.stats.failed_entries += 1;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(EntryCode::Group(code)) =
            self.entries.get(&key).and_then(|entry| entry.code.as_ref())
        {
            self.stats.compiled_calls = self.stats.compiled_calls.saturating_add(1);
            Some(code.run(memory))
        } else {
            None
        }
    }

    pub(crate) fn begin(
        &mut self,
        key: EntryKey,
    ) -> (
        Option<Instant>,
        Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    ) {
        self.stats.interpreted_calls = self.stats.interpreted_calls.saturating_add(1);
        let entry = self.entries.entry(key).or_default();
        entry.calls = entry.calls.saturating_add(1);
        let sample = entry.calls <= 4 || entry.calls.is_multiple_of(64);
        // Persistent interpreter cost justifies a larger compilation job. Small
        // regions remain useful while it runs; the size limit is still finite.
        let escalate =
            entry.block_regions && !entry.large_attempted && entry.estimated_cost >= LARGE_HOT_COST;
        let request = !self.busy
            && !self.cancel.is_cancelled()
            && entry.code.is_none()
            && (!entry.unavailable || escalate)
            && entry.calls >= 2
            && entry.estimated_cost >= HOT_COST;
        if request && escalate {
            entry.large_attempted = true;
        }
        (sample.then(Instant::now), request.then(Vec::new))
    }

    pub(crate) fn collect(
        &mut self,
        key: EntryKey,
        units: &mut Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
        unit: &ExecutionUnit<RegionedAbsoluteAddr>,
        instructions: &mut usize,
    ) {
        if units.is_none() {
            return;
        }
        *instructions = instructions.saturating_add(
            unit.blocks
                .values()
                .map(|b| b.instructions.len())
                .sum::<usize>(),
        );
        let unsupported = unit.register_map.values().any(|ty| ty.width() > 4096)
            || unit.blocks.values().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|inst| matches!(inst, celox_sir::SIRInstruction::Call { .. }))
            });
        let limit = if self.entries[&key].large_attempted {
            MAX_LARGE_INSTRUCTIONS
        } else {
            MAX_INSTRUCTIONS
        };
        if *instructions > limit || unsupported {
            *units = None;
            self.entries
                .get_mut(&key)
                .expect("entry was sampled")
                .unavailable = true;
            self.entries.get_mut(&key).unwrap().block_regions = true;
            self.stats.rejected_entries += 1;
        } else {
            units.as_mut().unwrap().push(unit.clone());
        }
    }

    pub(crate) fn finish(
        &mut self,
        key: EntryKey,
        start: Option<Instant>,
        units: Option<Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    ) {
        let entry = self.entries.get_mut(&key).expect("entry was sampled");
        if let Some(start) = start {
            let weight = if entry.calls <= 4 { 1 } else { 64 };
            entry.estimated_cost = entry
                .estimated_cost
                .saturating_add(start.elapsed().saturating_mul(weight));
        }
        // A region may have queued work or published this group while its
        // interpreted invocation was running. Never queue a second job behind
        // that work: `busy` represents exactly one outstanding completion.
        if let Some(units) =
            units.filter(|units| !units.is_empty() && !self.busy && entry.code.is_none())
        {
            if let Some(sender) = &self.requests {
                match sender.try_send(Request {
                    key,
                    units,
                    block: None,
                }) {
                    Ok(()) => self.busy = true,
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        self.cancel();
                    }
                    Err(mpsc::TrySendError::Full(_)) => {}
                }
            }
        }
    }

    pub(crate) fn has_regions(&self, group: EntryKey) -> bool {
        self.entries
            .get(&group)
            .is_some_and(|entry| entry.block_regions)
    }

    pub(crate) fn try_block(
        &mut self,
        group: EntryKey,
        unit_index: usize,
        source: &std::sync::Arc<RegionSource>,
        block: celox_sir::BlockId,
        registers: &mut crate::interpreter::Registers,
        memory: &mut [u64],
    ) -> Result<Option<celox_sir::BlockId>, crate::interpreter::InterpError> {
        let key = group.block(unit_index, block);
        // Poll publication without invoking a whole-group entry. Block keys
        // can only name block code, which needs the explicit register frame.
        let group_result = self.try_run(key, memory);
        debug_assert!(group_result.is_none());
        if let Some(EntryCode::Block(code)) =
            self.entries.get(&key).and_then(|entry| entry.code.as_ref())
        {
            code.run(memory, registers)?;
            self.stats.compiled_block_calls = self.stats.compiled_block_calls.saturating_add(1);
            self.stats.frame_bytes_transferred = self
                .stats
                .frame_bytes_transferred
                .saturating_add(code.transferred_bytes as u64);
            return Ok(Some(code.tail));
        }
        if self.entries.len() >= 65_536 && !self.entries.contains_key(&key) {
            return Ok(None);
        }
        let entry = self.entries.entry(key).or_default();
        entry.calls = entry.calls.saturating_add(1);
        if entry.calls >= 8
            && !entry.unavailable
            && !self.busy
            && !self.cancel.is_cancelled()
            && self.stats.code_bytes < MAX_CODE_BYTES
        {
            if let Some(block) = BlockRequest::new(source, block) {
                if let Some(sender) = &self.requests {
                    match sender.try_send(Request {
                        key,
                        units: Vec::new(),
                        block: Some(block),
                    }) {
                        Ok(()) => self.busy = true,
                        Err(mpsc::TrySendError::Disconnected(_)) => {
                            self.cancel();
                        }
                        Err(mpsc::TrySendError::Full(_)) => {}
                    }
                }
            } else {
                entry.unavailable = true;
            }
        }
        Ok(None)
    }
}

impl Drop for HotspotRuntime {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        tracing::debug!(stats = ?self.stats, "hotspot execution finished");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Simulator, backend::SimBackend};
    use num_bigint::BigUint;

    const WIDE: &str = r#"
module Top (
    clk: input clock,
    d: input logic<130>,
    q: output logic<130>,
) {
    always_ff (clk) { q = d; }
}
"#;

    #[test]
    fn mixed_entries_preserve_wide_unknowns_and_state_address() {
        let mut mixed = crate::SimulatorBuilder::<Simulator>::new(WIDE, "Top")
            .four_state(true)
            .hotspot_jit(true)
            .build_tiered()
            .unwrap();
        let mut reference = crate::SimulatorBuilder::<Simulator>::new(WIDE, "Top")
            .four_state(true)
            .build_interpreter()
            .unwrap();
        let pointer = mixed.backend.memory_as_ptr().0;
        let (clk, d, q) = (mixed.event("clk"), mixed.signal("d"), mixed.signal("q"));
        let (rclk, rd, rq) = (
            reference.event("clk"),
            reference.signal("d"),
            reference.signal("q"),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut iteration = 0u32;
        loop {
            let value = (BigUint::from(1u8) << 129usize) | BigUint::from(iteration);
            let mask = (BigUint::from(1u8) << 128usize) | BigUint::from(iteration & 0xff);
            mixed
                .modify(|io| io.set_four_state(d, value.clone(), mask.clone()))
                .unwrap();
            reference
                .modify(|io| io.set_four_state(rd, value, mask))
                .unwrap();
            mixed.tick(clk).unwrap();
            reference.tick(rclk).unwrap();
            assert_eq!(mixed.get_four_state(q), reference.get_four_state(rq));
            iteration += 1;
            let stats = mixed.backend.hotspot_stats().unwrap();
            if stats.compiled_calls >= 512 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no useful publication: {stats:?}"
            );
            if iteration.is_multiple_of(64) {
                std::thread::yield_now();
            }
        }
        assert_eq!(mixed.backend.memory_as_ptr().0, pointer);
        let stats = mixed.backend.hotspot_stats().unwrap();
        assert!(stats.interpreted_calls > 0 && stats.compiled_entries > 0);
        assert_eq!(stats.failed_entries, 0);
        assert!(
            !mixed.backend.is_compiled(),
            "no whole-image promotion occurred"
        );
        assert!(mixed.backend.cancel_background_compilation());
        assert!(!mixed.backend.cancel_background_compilation());
        let count = mixed.backend.hotspot_stats().unwrap().compiled_calls;
        for _ in 0..128 {
            mixed.tick(clk).unwrap();
        }
        assert!(mixed.backend.hotspot_stats().unwrap().compiled_calls > count);
    }

    #[test]
    fn never_policy_does_not_start_incremental_compilation() {
        let sim = crate::SimulatorBuilder::<Simulator>::new(WIDE, "Top")
            .hotspot_jit(true)
            .tier_promotion(crate::TierPromotion::Never)
            .build_tiered()
            .unwrap();
        assert!(sim.backend.hotspot_stats().is_none());
    }

    #[test]
    fn cancellation_and_code_budget_reject_an_already_completed_entry() {
        let sim = crate::SimulatorBuilder::<Simulator>::new(WIDE, "Top")
            .build_interpreter()
            .unwrap();
        let layout = sim.backend.layout();
        for cancelled in [false, true] {
            let options = SimulatorOptions::default();
            let mut runtime = HotspotRuntime::new(layout, &options).unwrap();
            let code = HotspotCode::compile(&[], layout, &options, &CompileCancel::new()).unwrap();
            let mut memory = vec![0u64; code.required_state_size.div_ceil(8)];
            let (sender, receiver) = mpsc::channel();
            runtime.completed = receiver;
            runtime.entries.entry(EntryKey::Comb).or_default();
            runtime.busy = true;
            runtime.polls = 63;
            sender
                .send(Completed {
                    key: EntryKey::Comb,
                    code: Ok(EntryCode::Group(Arc::new(code))),
                    elapsed: Duration::ZERO,
                })
                .unwrap();
            if cancelled {
                runtime.cancel();
            } else {
                runtime.stats.code_bytes = MAX_CODE_BYTES;
            }
            assert!(runtime.try_run(EntryKey::Comb, &mut memory).is_none());
            assert_eq!(runtime.stats.compiled_entries, 0);
            assert_eq!(runtime.stats.compiled_calls, 0);
            assert_eq!(runtime.stats.rejected_entries, usize::from(!cancelled));
        }
    }

    #[test]
    fn admission_needs_observed_cost_and_does_not_duplicate_an_inflight_job() {
        let sim = crate::SimulatorBuilder::<Simulator>::new(WIDE, "Top")
            .build_interpreter()
            .unwrap();
        let mut runtime =
            HotspotRuntime::new(sim.backend.layout(), &SimulatorOptions::default()).unwrap();
        assert!(runtime.begin(EntryKey::Comb).1.is_none());
        runtime
            .entries
            .get_mut(&EntryKey::Comb)
            .unwrap()
            .estimated_cost = HOT_COST;
        assert!(runtime.begin(EntryKey::Comb).1.is_some());
        runtime.busy = true;
        assert!(runtime.begin(EntryKey::Comb).1.is_none());
        runtime.busy = false;
        runtime
            .entries
            .get_mut(&EntryKey::Comb)
            .unwrap()
            .unavailable = true;
        assert!(runtime.begin(EntryKey::Comb).1.is_none());
    }

    #[test]
    fn hotspot_finish_does_not_queue_behind_a_region_or_recompile_published_code() {
        let sim = crate::SimulatorBuilder::<Simulator>::new(WIDE, "Top")
            .build_interpreter()
            .unwrap();
        let options = SimulatorOptions::default();
        let mut runtime = HotspotRuntime::new(sim.backend.layout(), &options).unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        runtime.requests = Some(sender);
        runtime.begin(EntryKey::Comb);
        runtime.busy = true;
        let unit = ExecutionUnit {
            entry_block_id: celox_sir::BlockId(0),
            blocks: [(
                celox_sir::BlockId(0),
                celox_sir::BasicBlock {
                    id: celox_sir::BlockId(0),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: celox_sir::SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: Default::default(),
        };
        runtime.finish(EntryKey::Comb, None, Some(vec![unit.clone()]));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        runtime.busy = false;
        let entry = runtime.entries.get_mut(&EntryKey::Comb).unwrap();
        entry.estimated_cost = LARGE_HOT_COST;
        entry.code = Some(EntryCode::Group(Arc::new(
            HotspotCode::compile(
                std::slice::from_ref(&unit),
                sim.backend.layout(),
                &options,
                &CompileCancel::new(),
            )
            .unwrap(),
        )));
        assert!(runtime.begin(EntryKey::Comb).1.is_none());
        runtime.finish(EntryKey::Comb, None, Some(vec![unit]));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}
