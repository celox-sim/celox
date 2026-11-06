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

use super::{
    SimulatorErrorCode,
    compile_cancel::CompileCancel,
    native::backend::{HotspotCode, PreparedHotspotScheduler},
};
use crate::{
    SimulatorOptions,
    ir::{ExecutionUnit, RegionedAbsoluteAddr},
};

pub(crate) const SCRATCH_BYTES: usize = 1024 * 1024;
const MAX_INSTRUCTIONS: usize = 32_768;
const MAX_LARGE_INSTRUCTIONS: usize = 1_048_576;
const LARGE_HOT_COST: Duration = Duration::from_millis(500);
// Large four-state RTL needs room for both Comb and its compiled scheduler
// (including outlined kernels), in addition to startup block regions.
const MAX_CODE_BYTES: usize = 64 * 1024 * 1024;
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

enum Work {
    Group(Vec<ExecutionUnit<RegionedAbsoluteAddr>>),
    Block(BlockRequest),
    /// A scheduler, and the inline budget of one still to be prepared.
    Scheduler(SchedulerState, Arc<HotspotCode>, Option<usize>),
}

struct Request {
    key: EntryKey,
    work: Work,
}

struct Completed {
    key: EntryKey,
    code: Result<EntryCode, String>,
    elapsed: Duration,
    /// A scheduler whose inline budget outlined units: a fully inlined one
    /// can replace it.
    budgeted: bool,
}

type Prepared = Result<PreparedHotspotScheduler, crate::SimulatorError>;

enum SchedulerState {
    Pending(Arc<crate::ir::LaidOutProgram>),
    /// Prepared on its own thread, so the simulation keeps running.
    Preparing(mpsc::Receiver<(Prepared, Duration)>),
    Prepared(Box<PreparedHotspotScheduler>),
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
    scheduler: Option<SchedulerState>,
    scheduler_index: Option<usize>,
    /// The program of the scheduler, while a later stage may still replace it.
    scheduler_program: Option<Arc<crate::ir::LaidOutProgram>>,
    preparer: Option<JoinHandle<()>>,
    /// The inline budget of the first scheduler stage.
    inline_budget: usize,
    /// Scheduler stages published.
    scheduler_stages: usize,
    options: Arc<SimulatorOptions>,
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
        let options = Arc::new(options.clone());
        let worker_options = Arc::clone(&options);
        let worker = std::thread::Builder::new()
            .name("celox-jit-hotspot".into())
            .spawn(move || {
                let options = worker_options;
                while let Ok(request) = receiver.recv() {
                    if worker_cancel.is_cancelled() {
                        break;
                    }
                    let start = Instant::now();
                    let mut budgeted = false;
                    let code = match request.work {
                        Work::Scheduler(state, comb, inline_budget) => {
                            let prepared = match state {
                                SchedulerState::Pending(program) => PreparedHotspotScheduler::new(
                                    program,
                                    &options,
                                    &worker_cancel,
                                    inline_budget,
                                )
                                .map(Box::new),
                                SchedulerState::Prepared(prepared) => Ok(prepared),
                                SchedulerState::Preparing(_) => {
                                    unreachable!("only prepared schedulers are submitted")
                                }
                            };
                            prepared
                                .and_then(|prepared| {
                                    budgeted = prepared.budgeted;
                                    HotspotCode::compile_scheduler(
                                        *prepared,
                                        comb,
                                        &options,
                                        &worker_cancel,
                                    )
                                })
                                .map(Arc::new)
                                .map(EntryCode::Group)
                        }
                        Work::Block(block) => block
                            .compile(&layout, &options, &worker_cancel)
                            .map(EntryCode::Block),
                        Work::Group(units) => {
                            HotspotCode::compile(&units, &layout, &options, &worker_cancel)
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
                            budgeted,
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
            scheduler: None,
            scheduler_index: None,
            scheduler_program: None,
            preparer: None,
            inline_budget: crate::scheduler::INLINE_BUDGET_INSTRUCTIONS,
            scheduler_stages: 0,
            options,
        })
    }

    pub(crate) fn prepare_scheduler(&mut self, program: &crate::ir::LaidOutProgram) {
        self.scheduler_index = program
            .runtime()
            .runtime_schema
            .scheduler
            .as_ref()
            .and_then(|s| s.unit);
        if self.scheduler_index.is_some() {
            let program = Arc::new(program.clone());
            self.scheduler_program = Some(Arc::clone(&program));
            self.scheduler = Some(SchedulerState::Pending(program));
        }
    }

    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn scheduler_is_prepared_for_test(&self) -> bool {
        self.scheduler_index.is_some()
    }

    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn request_comb_for_test(&mut self, units: &[ExecutionUnit<RegionedAbsoluteAddr>]) {
        if !self.busy
            && self
                .entries
                .get(&EntryKey::Comb)
                .is_none_or(|entry| entry.code.is_none())
        {
            self.begin(EntryKey::Comb);
            self.finish(EntryKey::Comb, None, Some(units.to_vec()));
        }
    }

    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn set_inline_budget_for_test(&mut self, budget: usize) {
        self.inline_budget = budget;
    }

    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn scheduler_stages_for_test(&self) -> usize {
        self.scheduler_stages
    }

    /// Complete the scheduler entry's job with a failure, or with code the
    /// code budget rejects, as the worker would.
    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn finish_scheduler_for_test(
        &mut self,
        layout: &crate::backend::MemoryLayout,
        rejected: bool,
    ) {
        let key = EntryKey::Process(self.scheduler_index.expect("scheduler prepared"));
        let code = if rejected {
            let code = HotspotCode::compile(&[], layout, &self.options, &CompileCancel::new())
                .map_err(|error| error.to_string())
                .unwrap();
            self.stats.code_bytes = MAX_CODE_BYTES;
            Ok(EntryCode::Group(Arc::new(code)))
        } else {
            Err("injected".to_string())
        };
        let (sender, receiver) = mpsc::channel();
        self.completed = receiver;
        self.scheduler = None;
        self.busy = true;
        self.polls = 63;
        sender
            .send(Completed {
                key,
                code,
                elapsed: Duration::ZERO,
                budgeted: true,
            })
            .unwrap();
    }

    #[cfg(all(
        test,
        feature = "systemverilog",
        target_arch = "x86_64",
        not(feature = "arm64-codegen")
    ))]
    pub(crate) fn holds_scheduler_program_for_test(&self) -> bool {
        self.scheduler_program.is_some()
    }

    pub(crate) fn has_compiled_scheduler(&self) -> bool {
        self.scheduler_index.is_some_and(|index| {
            self.entries
                .get(&EntryKey::Process(index))
                .is_some_and(|entry| entry.code.is_some())
        })
    }

    /// Plan and allocate the first scheduler stage while the single worker
    /// compiles an admitted large Comb. Small groups keep the original lazy
    /// worker path, so short simulations do no extra planning.
    ///
    /// With another CPU it is prepared on its own thread and the simulation
    /// keeps running. On a single CPU the interpreter would only slow it
    /// down: compiled slots then start soonest when the simulation thread
    /// prepares it.
    fn prepare_large_scheduler(&mut self) {
        let Some(state) = self.scheduler.take() else {
            return;
        };
        let SchedulerState::Pending(program) = state else {
            self.scheduler = Some(state);
            return;
        };
        let budget = Some(self.inline_budget);
        if !single_cpu() {
            self.prepare_scheduler_stage(program, budget);
            return;
        }
        let start = Instant::now();
        let prepared = PreparedHotspotScheduler::new(program, &self.options, &self.cancel, budget);
        let elapsed = start.elapsed();
        self.stats.compile_time += elapsed;
        match prepared {
            Ok(prepared) => {
                tracing::debug!(
                    ?elapsed,
                    budgeted = prepared.budgeted,
                    "hotspot scheduler prepared"
                );
                self.scheduler = Some(SchedulerState::Prepared(Box::new(prepared)));
            }
            Err(error) => {
                tracing::debug!(%error, "hotspot scheduler preparation failed");
                self.scheduler_failed();
            }
        }
    }

    fn prepare_scheduler_stage(
        &mut self,
        program: Arc<crate::ir::LaidOutProgram>,
        inline_budget: Option<usize>,
    ) {
        if let Some(previous) = self.preparer.take() {
            // It already sent its result, which is what started this stage.
            let _ = previous.join();
        }
        let (sender, receiver) = mpsc::channel();
        let options = Arc::clone(&self.options);
        let cancel = self.cancel.clone();
        // The final stage is an optimization of running code. On a single
        // CPU it must not halve the speed of the stage it replaces.
        let background = inline_budget.is_none() && single_cpu();
        let spawned = std::thread::Builder::new()
            .name("celox-jit-hotspot-scheduler".into())
            .spawn(move || {
                if background {
                    lower_priority();
                }
                let start = Instant::now();
                let prepared =
                    PreparedHotspotScheduler::new(program, &options, &cancel, inline_budget);
                let _ = sender.send((prepared, start.elapsed()));
            });
        match spawned {
            Ok(handle) => {
                self.preparer = Some(handle);
                self.scheduler = Some(SchedulerState::Preparing(receiver));
            }
            Err(error) => {
                tracing::debug!(%error, "hotspot scheduler preparation did not start");
                self.scheduler_failed();
            }
        }
    }

    fn scheduler_failed(&mut self) {
        self.scheduler_program = None;
        let key = EntryKey::Process(self.scheduler_index.unwrap());
        let entry = self.entries.entry(key).or_default();
        // A published earlier stage keeps running.
        if entry.code.is_none() {
            self.stats.failed_entries += 1;
            entry.unavailable = true;
        }
    }

    /// Adopt a finished preparation; `false` while it is still running.
    fn poll_preparation(&mut self) -> bool {
        let Some(SchedulerState::Preparing(receiver)) = &self.scheduler else {
            return true;
        };
        match receiver.try_recv() {
            Ok((prepared, elapsed)) => {
                self.stats.compile_time += elapsed;
                match prepared {
                    Ok(prepared) => {
                        tracing::debug!(
                            ?elapsed,
                            budgeted = prepared.budgeted,
                            "hotspot scheduler prepared"
                        );
                        self.scheduler = Some(SchedulerState::Prepared(Box::new(prepared)));
                        true
                    }
                    Err(error) => {
                        tracing::debug!(%error, "hotspot scheduler preparation failed");
                        self.scheduler = None;
                        self.scheduler_failed();
                        false
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.scheduler = None;
                self.scheduler_failed();
                false
            }
        }
    }

    fn request_scheduler(&mut self) {
        if self.busy
            || self.cancel.is_cancelled()
            || self.scheduler.is_none()
            || !self.poll_preparation()
            || self.scheduler.is_none()
        {
            return;
        }
        let Some(EntryCode::Group(comb)) = self
            .entries
            .get(&EntryKey::Comb)
            .and_then(|entry| entry.code.as_ref())
        else {
            return;
        };
        let Some(sender) = &self.requests else {
            return;
        };
        let request = Request {
            key: EntryKey::Process(self.scheduler_index.unwrap()),
            work: Work::Scheduler(
                self.scheduler.take().unwrap(),
                Arc::clone(comb),
                Some(self.inline_budget),
            ),
        };
        match sender.try_send(request) {
            Ok(()) => self.busy = true,
            Err(mpsc::TrySendError::Full(request)) => {
                let Work::Scheduler(state, ..) = request.work else {
                    unreachable!("only scheduler jobs are submitted here");
                };
                self.scheduler = Some(state);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                self.cancel();
            }
        }
    }

    pub(crate) fn stats(&self) -> HotspotStats {
        self.stats.clone()
    }

    pub(crate) fn cancel(&mut self) -> bool {
        let active = !self.cancel.is_cancelled();
        self.cancel.cancel();
        self.requests.take();
        self.scheduler = None;
        self.scheduler_program = None;
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
        self.prepare(key, memory.len() * 8)
            .map(|code| code.run(memory))
    }

    pub(crate) fn prepare(
        &mut self,
        key: EntryKey,
        state_size: usize,
    ) -> Option<&Arc<HotspotCode>> {
        self.polls = self.polls.wrapping_add(1);
        if self.busy && self.polls.is_multiple_of(64) {
            match self.completed.try_recv() {
                Ok(completed) => {
                    self.busy = false;
                    self.stats.compile_time += completed.elapsed;
                    let scheduler =
                        Some(completed.key) == self.scheduler_index.map(EntryKey::Process);
                    if scheduler && completed.code.is_err() {
                        // No later stage can follow a failed one.
                        self.scheduler_program = None;
                    }
                    let entry = self.entries.entry(completed.key).or_default();
                    // A later scheduler stage replaces the published one.
                    let replaced = entry
                        .code
                        .as_ref()
                        .map_or(0, |code| code.native().code_size);
                    match completed.code {
                        Ok(code)
                            if code.native().required_state_size <= state_size
                                && (self.stats.code_bytes - replaced)
                                    .saturating_add(code.native().code_size)
                                    <= MAX_CODE_BYTES =>
                        {
                            tracing::debug!(key = ?completed.key, elapsed = ?completed.elapsed,
                                bytes = code.native().code_size, replaced_bytes = replaced,
                                "hotspot entry published");
                            self.stats.code_bytes =
                                self.stats.code_bytes - replaced + code.native().code_size;
                            if entry.code.is_none() {
                                self.stats.compiled_entries += 1;
                            }
                            entry.code = Some(code);
                            if scheduler {
                                self.scheduler_stages += 1;
                                match self.scheduler_program.take() {
                                    // Inline what the budget outlined: the
                                    // final scheduler is the fastest. On a
                                    // single CPU only when it can yield.
                                    Some(program)
                                        if completed.budgeted
                                            && (CAN_LOWER_PRIORITY || !single_cpu()) =>
                                    {
                                        self.prepare_scheduler_stage(program, None);
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Ok(code) => {
                            tracing::debug!(key = ?completed.key,
                                required_state_bytes = code.native().required_state_size,
                                available_state_bytes = state_size,
                                code_bytes = code.native().code_size,
                                published_bytes = self.stats.code_bytes,
                                code_budget_bytes = MAX_CODE_BYTES,
                                "hotspot publication exceeded its memory or code budget");
                            entry.unavailable = true;
                            self.stats.rejected_entries += 1;
                            if scheduler {
                                self.scheduler_program = None;
                            }
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
        self.request_scheduler();
        if let Some(EntryCode::Group(code)) =
            self.entries.get(&key).and_then(|entry| entry.code.as_ref())
        {
            self.stats.compiled_calls = self.stats.compiled_calls.saturating_add(1);
            Some(code)
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
            tracing::debug!(
                ?key,
                instructions = *instructions,
                limit,
                unsupported,
                "hotspot group deferred to interpreter or block regions"
            );
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
        let prepare_scheduler = key == EntryKey::Comb && entry.large_attempted;
        // A region may have queued work or published this group while its
        // interpreted invocation was running. Never queue a second job behind
        // that work: `busy` represents exactly one outstanding completion.
        if let Some(units) =
            units.filter(|units| !units.is_empty() && !self.busy && entry.code.is_none())
        {
            if let Some(sender) = &self.requests {
                match sender.try_send(Request {
                    key,
                    work: Work::Group(units),
                }) {
                    Ok(()) => {
                        self.busy = true;
                        if prepare_scheduler {
                            self.prepare_large_scheduler();
                        }
                    }
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
                        work: Work::Block(block),
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

pub(crate) fn single_cpu() -> bool {
    std::thread::available_parallelism().map_or(true, |cpus| cpus.get() == 1)
}

/// Whether [`lower_priority`] has an effect on this host.
const CAN_LOWER_PRIORITY: bool = cfg!(target_os = "linux");

/// Let the calling thread yield the CPU to the simulation.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // Safety: plain system calls on the calling thread; a failure leaves its
    // priority unchanged, which is only slower.
    unsafe {
        let thread = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, thread, 10);
    }
}

impl Drop for HotspotRuntime {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(preparer) = self.preparer.take() {
            let _ = preparer.join();
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

    #[cfg(feature = "systemverilog")]
    #[test]
    fn hotspot_scheduler_preserves_wide_unknowns_and_derived_edges() {
        const SOURCE: &str = r#"`timescale 1ns/1ns
module Top;
  logic clk = 0, en = 0;
  wire gclk = clk & en;
  logic [129:0] q = 0, g = 0;
  logic [7:0] seen = 0;
  always #5 clk = ~clk;
  always #37 en = ~en;
  always_ff @(posedge clk) q <= {2'bxz, q[127:0] + 128'd1};
  always_ff @(posedge gclk) g <= {q[129:128], g[127:0] + q[127:0]};
  always @(posedge gclk) seen = seen + 8'd1;
endmodule
"#;
        let directory = tempfile::tempdir().unwrap();
        for recording in [false, true] {
            let mixed_vcd = directory.path().join("mixed.vcd");
            let reference_vcd = directory.path().join("reference.vcd");
            let builder = || {
                crate::Simulation::from_sv_sources(
                    vec![(SOURCE, std::path::Path::new("hotspot-scheduler.sv"))],
                    "Top",
                )
                .four_state(true)
                .settle_partition(crate::SettlePartition::Always)
            };
            let mixed_builder = if recording {
                builder().vcd(&mixed_vcd)
            } else {
                builder()
            };
            let reference_builder = if recording {
                builder().vcd(&reference_vcd)
            } else {
                builder()
            };
            let mut mixed = mixed_builder.hotspot_jit(true).build_tiered().unwrap();
            let mut reference = reference_builder.build_interpreter().unwrap();
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut time = 0;
            loop {
                time += 43;
                mixed.run_until(time).unwrap();
                reference.run_until(time).unwrap();
                for name in ["q", "g", "seen"] {
                    assert_eq!(
                        mixed.get_four_state(mixed.signal(name)),
                        reference.get_four_state(reference.signal(name)),
                        "{name} at {time}"
                    );
                }
                let stats = mixed.hotspot_stats().unwrap();
                assert_eq!(stats.failed_entries, 0, "{stats:?}");
                if mixed.scheduler_stats().compiled_slots >= 256 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "no scheduler publication: {stats:?}"
                );
                std::thread::yield_now();
            }
            // Restore a state captured after actual native scheduler execution into
            // a fresh mixed executor. The checkpoint contains no JIT code or frames.
            let checkpoint = mixed.save_state().unwrap();
            let mut resumed = builder()
                .hotspot_jit(true)
                .defer_initialization(true)
                .build_tiered()
                .unwrap();
            resumed.load_state(&checkpoint).unwrap();
            assert_eq!(resumed.time(), mixed.time());
            // Cancelling future work must not invalidate already linked code.
            mixed.simulator.backend.cancel_background_compilation();
            mixed.run_until(time + 10_000).unwrap();
            reference.run_until(time + 10_000).unwrap();
            resumed.run_until(time + 10_000).unwrap();
            for name in ["q", "g", "seen"] {
                let expected = reference.get_four_state(reference.signal(name));
                assert_eq!(mixed.get_four_state(mixed.signal(name)), expected);
                assert_eq!(resumed.get_four_state(resumed.signal(name)), expected);
            }
            if recording {
                mixed.flush_vcd().unwrap();
                reference.flush_vcd().unwrap();
                let mixed = std::fs::read_to_string(&mixed_vcd).unwrap();
                let reference = std::fs::read_to_string(&reference_vcd).unwrap();
                assert_eq!(
                    mixed.split_once("$enddefinitions").unwrap().1,
                    reference.split_once("$enddefinitions").unwrap().1
                );
            }
        }
    }

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
                    budgeted: false,
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
