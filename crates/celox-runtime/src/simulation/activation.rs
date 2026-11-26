//! Dynamic executions of a stackless kernel. Forks share lexical cells, but
//! each child owns its control slots and compiler temporaries.
use super::{SimulationState, wait::wake_in_region};
use crate::{
    SignalRef, SimulatorErrorCode,
    backend::SimBackend,
    timing::{Halt, Value, read},
};
use celox_design::ForkJoin;
use frame_image::{FrameImage, ImageLayout};
use std::{cmp::Reverse, sync::Arc};

pub mod frame_image;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProcessControl {
    /// Whether a process handle names the activation: then it is
    /// [`Self::handle`], a key of
    /// [`crate::timing::ProcessState::process_handles`].
    pub exposed: bool,
    /// The heap object of the activation's process handle.
    #[serde(default)]
    pub handle: Option<u32>,
    /// A compiler-owned handle records this process (see
    /// `ProcessOp::SelfHandleInternal`): it may be controlled through
    /// that handle, but a killed one's activation stays out of reuse,
    /// since its armed variable may still name it.
    #[serde(default)]
    pub internal: bool,
    pub random: Option<u64>,
    pub killed: bool,
    pub suspended: Option<SuspendedProcess>,
    pub awaiting: Option<usize>,
    pub observes: Vec<usize>,
    pub level_wait: bool,
    /// The named events the process is blocked on.
    #[serde(default)]
    pub events: Vec<u32>,
    /// The named events whose `triggered` state the predicate of its level
    /// wait read: a trigger of one checks the predicate again.
    #[serde(default)]
    pub observed_events: Vec<u32>,
    /// The semaphore whose keys the process is blocked on.
    #[serde(default)]
    pub semaphore: Option<u32>,
    /// The mailbox the process waits to put into or take from.
    #[serde(default)]
    pub mailbox: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SuspendedProcess {
    pub runnable: bool,
    pub wake_time: Option<u64>,
    pub wait_word: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Activation {
    #[serde(default)]
    pub control: ProcessControl,
    pub kernel: usize,
    pub parent: Option<usize>,
    /// The private slots of its frame, as the bytes of their planes (see
    /// [`FrameImage`]).
    pub private: FrameImage,
    pub locals: Vec<Option<usize>>,
    pub live: bool,
    pub pending: Vec<usize>,
    pub join: Option<(ForkJoin, Vec<usize>)>,
}
impl Activation {
    fn root(kernel: usize) -> Self {
        Self {
            control: ProcessControl::default(),
            kernel,
            parent: None,
            private: FrameImage::default(),
            locals: Vec::new(),
            live: true,
            pending: Vec::new(),
            join: None,
        }
    }
}
/// The bytes of the resume slot of a function unit.
const UNIT_RESUME_BYTES: usize = celox_design::PROCESS_RESUME_WIDTH.div_ceil(8);

fn write<B: SimBackend>(backend: &mut B, signal: SignalRef, value: &Value) {
    backend.set_four_state(signal, value.0.clone(), value.1.clone());
}
impl<B: SimBackend> SimulationState<B> {
    /// Put back the frames of the calls `process` suspended inside, which
    /// other code may have overwritten since: a resumed chain finds each
    /// frame where it left it.
    pub(super) fn restore_call_stack(&mut self, backend: &mut B, process: usize) {
        if let Some(saved) = self.process_state.call_stacks.remove(&process) {
            backend.restore_call_stack(&saved.stack);
            self.stats.call_stack_restored_bytes += saved.stack.len() as u64;
        }
    }

    /// Keep the frames of the calls `process` suspended inside until it
    /// runs again, and clear its [`celox_design::ProcessSlots::call_top`].
    /// A process that finished the simulation inside a call never resumes,
    /// so its frames are dropped.
    pub(super) fn save_call_stack(&mut self, backend: &mut B, process: usize) {
        let Some(slot) = self.model.processes[process].call_top else {
            return;
        };
        let top: u32 = backend.get_as(slot);
        if top == 0 {
            return;
        }
        let status: u8 = backend.get_as(self.model.processes[process].status);
        if celox_design::ProcessStatus::from_code(status)
            == Some(celox_design::ProcessStatus::Finish)
        {
            backend.set(slot, 0u32);
            return;
        }
        let mut stack = Vec::with_capacity(top as usize);
        backend.save_call_stack(top as usize, &mut stack);
        backend.set(slot, 0u32);
        self.stats.call_stack_saves += 1;
        self.stats.call_stack_saved_bytes += u64::from(top);
        let resume = backend.get_as(self.model.processes[process].resume);
        self.process_state
            .call_stacks
            .insert(process, super::SavedCalls { resume, stack });
    }

    pub(super) fn restore_activation(&mut self, backend: &mut B, process: usize) -> usize {
        if self.process_state.activations.is_empty() {
            if !self.model.processes.iter().any(|p| p.frame.is_some()) {
                return process;
            }
            self.process_state.activations = (0..self.kernel_count).map(Activation::root).collect();
            self.rebuild_activation_reuse();
        }
        let activation = &mut self.process_state.activations[process];
        if let Some(frame) = &self.model.processes[process].frame {
            if activation.private.is_empty() {
                let layout = Arc::new(ImageLayout::new(
                    &frame.private,
                    backend.layout().four_state,
                ));
                activation.private = FrameImage::capture(&layout, backend);
                activation.locals.resize(frame.locals.len(), None);
            } else {
                activation.private.restore(backend);
            }
            debug_assert_frame(backend, &frame.private, &activation.private);
            for (&slot, cell) in frame.locals.iter().zip(&activation.locals) {
                if let Some(cell) = cell {
                    write(backend, slot, &self.process_state.cells[*cell]);
                }
            }
        }
        activation.kernel
    }

    pub(super) fn save_activation(&mut self, backend: &B, process: usize) {
        let Some(frame) = &self.model.processes[process].frame else {
            return;
        };
        let activation = &mut self.process_state.activations[process];
        activation.private.save(backend);
        debug_assert_frame(backend, &frame.private, &activation.private);
        for (&slot, cell) in frame.locals.iter().zip(&activation.locals) {
            if let Some(cell) = cell {
                self.process_state.cells[*cell] = read(backend, slot);
            }
        }
    }

    pub(super) fn allocate_local(&mut self, process: usize, index: usize) -> Result<(), Halt> {
        if self.model.processes[process].frame.is_none() {
            return Ok(());
        }
        if self.process_state.cells.len() >= 1024
            && self.process_state.cells.len().is_multiple_of(1024)
        {
            self.collect_activation_cells();
        }
        let locals = &mut self.process_state.activations[process].locals;
        let target = locals
            .get_mut(index)
            .ok_or(SimulatorErrorCode::InternalError)?;
        *target = Some(self.process_state.cells.len());
        self.process_state.cells.push(Value::default());
        Ok(())
    }

    pub(super) fn fork_activation(
        &mut self,
        backend: &mut B,
        process: usize,
        encoded: u64,
    ) -> Result<bool, Halt> {
        // Join code 3 is a `join_none` whose children are processes of the
        // analysis (`celox_frontend_core::process::fork_mode`): the sampler
        // of a covergroup instance a procedure constructs (19.3). They draw
        // no seed from the forking process (18.14.1) and are no children of
        // it, which its `wait fork`, `disable fork` and `kill()` do not see
        // (9.6.1, 9.6.3, 9.7).
        let (join, unseeded) = match (encoded >> 32) & 0b11 {
            0 => (ForkJoin::All, false),
            1 => (ForkJoin::Any, false),
            2 => (ForkJoin::None, false),
            _ => (ForkJoin::None, true),
        };
        // A function called by a declaration initializer may fork a
        // background process only from an initial or always procedure
        // (IEEE 1800-2023 13.4.4), checked when it forks.
        if !unseeded && self.model.processes[process].kind == celox_design::ProcessKind::Initializer
        {
            return Err(SimulatorErrorCode::Runtime {
                message: "background process scheduled by a declaration initializer; IEEE \
                          1800-2023 13.4.4 requires an initial/always process"
                    .into(),
                signals: Vec::new(),
            }
            .into());
        }
        if !unseeded && self.model.processes[process].assertion {
            return Err(assertion_action_fork());
        }
        let first = (encoded & 0xffff) as usize;
        let count = ((encoded >> 16) & 0xffff) as usize;
        // A fork of a function unit the process is suspended inside: each
        // child continues the call the kernel is suspended in, through a
        // copy of the parent's frames whose forking unit resumes at the
        // child's entry (see `celox_design::PROCESS_FORK_RESUME_SHIFT`).
        let unit_resume = (encoded >> celox_design::PROCESS_FORK_RESUME_SHIFT) as usize;
        let frames = if unit_resume == 0 {
            None
        } else {
            let slot = self.model.processes[process]
                .call_top
                .ok_or(SimulatorErrorCode::InternalError)?;
            let top: u32 = backend.get_as(slot);
            if unit_resume + UNIT_RESUME_BYTES > top as usize {
                return Err(SimulatorErrorCode::InternalError.into());
            }
            let mut stack = Vec::with_capacity(top as usize);
            backend.save_call_stack(top as usize, &mut stack);
            let resume = backend.get_as::<u64>(self.model.processes[process].resume);
            Some((stack, resume))
        };
        self.save_activation(backend, process);
        let frame = self.model.processes[process]
            .frame
            .as_ref()
            .ok_or(SimulatorErrorCode::InternalError)?;
        let layout = self.process_state.activations[process]
            .private
            .layout()
            .ok_or(SimulatorErrorCode::InternalError)?;
        let resume = layout
            .index_of(self.model.processes[process].resume)
            .ok_or(SimulatorErrorCode::InternalError)?;
        let status = layout
            .index_of(self.model.processes[process].status)
            .ok_or(SimulatorErrorCode::InternalError)?;
        let random_slot = frame.random;
        let random = layout
            .index_of(random_slot)
            .ok_or(SimulatorErrorCode::InternalError)?;
        let raw = backend.get_as::<u64>(random_slot);
        let mut seed = if raw == 0 { frame.seed } else { raw as u32 };
        let mut children = Vec::with_capacity(count);
        for entry in first..first + count {
            let mut child = self.process_state.activations[process].clone();
            // A child of the analysis keeps the generator state of the
            // parent, which it does not draw from.
            if !unseeded {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                child
                    .private
                    .set(random, &((u64::from(seed) | (1 << 32)).into(), 0u8.into()));
            }
            child.control = ProcessControl::default();
            child.live = true;
            // Linked below, by `set_parent`.
            child.parent = None;
            child.pending.clear();
            child.join = None;
            if frames.is_none() {
                child
                    .private
                    .set(resume, &((entry as u64).into(), 0u8.into()));
            }
            child.private.set(status, &(0u8.into(), 0u8.into()));
            let id = match self.take_reusable_activation() {
                Some(id) => {
                    self.set_parent(id, None);
                    self.process_state.activations[id] = child;
                    self.model.processes[id] = self.model.processes[process].clone();
                    id
                }
                None => {
                    self.process_state.activations.push(child);
                    self.activation_children.push(0);
                    self.model
                        .processes
                        .push(self.model.processes[process].clone());
                    self.process_state.activations.len() - 1
                }
            };
            self.set_parent(id, (!unseeded).then_some(process));
            if let Some((stack, resume)) = &frames {
                let mut stack = stack.clone();
                stack[unit_resume..unit_resume + UNIT_RESUME_BYTES]
                    .copy_from_slice(&(entry as u64).to_le_bytes()[..UNIT_RESUME_BYTES]);
                self.process_state.call_stacks.insert(
                    id,
                    super::SavedCalls {
                        resume: *resume,
                        stack,
                    },
                );
            } else {
                self.process_state.call_stacks.remove(&id);
            }
            children.push(id);
        }
        if count != 0 && !unseeded {
            backend.set(random_slot, u64::from(seed) | (1 << 32));
        }
        self.process_state.activations[process]
            .pending
            .extend(&children);
        if join == ForkJoin::None || children.is_empty() {
            return Ok(true);
        }
        self.process_state.activations[process].join = Some((join, children));
        Ok(false)
    }

    /// A real HDL suspension or termination starts the children created since
    /// the last such boundary. Host requests, NBA staging and budget yields do not.
    pub(super) fn release_children(&mut self, process: usize) {
        if self.process_state.activations.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.process_state.activations[process].pending);
        // They start now, before the processes ready already (see
        // `Self::next_spawned`).
        for &child in pending.iter().rev() {
            self.spawned.push_front(child);
        }
        for child in pending {
            let state = &mut self.process_state;
            wake_in_region(
                &self.model.processes,
                &mut state.ready,
                &mut state.reactive,
                child,
            );
        }
    }

    /// Only immediate, still-live children participate, even when an already
    /// completed child left running descendants (IEEE 1800-2023 9.6.1).
    pub(super) fn wait_for_children(&mut self, process: usize) -> bool {
        let children: Vec<_> = self
            .process_state
            .activations
            .iter()
            .enumerate()
            .filter_map(|(id, activation)| {
                (activation.live && activation.parent == Some(process)).then_some(id)
            })
            .collect();
        if children.is_empty() {
            return true;
        }
        self.process_state.activations[process].join = Some((ForkJoin::All, children));
        false
    }

    /// Follow ancestry through completed parents too (IEEE 1800-2023 9.6.3).
    /// Remove scheduler entries before releasing IDs for reuse.
    pub(super) fn disable_children(&mut self, process: usize) -> Vec<usize> {
        if self.process_state.activations.is_empty() {
            return Vec::new();
        }
        let mut cancelled = vec![false; self.process_state.activations.len()];
        let mut pending = vec![process];
        while let Some(parent) = pending.pop() {
            for (id, activation) in self.process_state.activations.iter().enumerate() {
                if activation.parent == Some(parent) && !cancelled[id] {
                    cancelled[id] = true;
                    pending.push(id);
                }
            }
        }
        let live_cancelled: Vec<usize> = cancelled
            .iter()
            .enumerate()
            .filter_map(|(id, &cancelled)| {
                (cancelled && self.process_state.activations[id].live).then_some(id)
            })
            .collect();
        let state = &mut self.process_state;
        for (id, activation) in state.activations.iter_mut().enumerate() {
            activation.pending.retain(|&child| !cancelled[child]);
            if cancelled[id] {
                activation.control.killed |= activation.live;
                activation.control.suspended = None;
                activation.control.awaiting = None;
                activation.live = false;
                activation.join = None;
                state.waiters.remove(id);
                state.ready.remove(&id);
                state.deferred_ready.remove(&id);
                state.reactive.remove(&id);
                state.observed.remove(&id);
                state.inactive.remove(&id);
                state.re_inactive.remove(&id);
            }
        }
        self.process_wakeups
            .retain(|std::cmp::Reverse((_, id))| !cancelled[*id]);
        // Already captured NBA values are independent scheduler work.
        let ids: Vec<usize> = cancelled
            .iter()
            .enumerate()
            .filter_map(|(id, &cancelled)| cancelled.then_some(id))
            .collect();
        for &id in &ids {
            self.retire_activation(id);
        }
        self.release_for_reuse(&ids, true);
        for &id in &live_cancelled {
            self.process_changed(id);
        }
        live_cancelled
    }

    pub(super) fn end_activation(&mut self, process: usize) {
        if self.process_state.activations.is_empty()
            || self.model.processes[process].frame.is_none()
        {
            return;
        }
        self.process_state.activations[process].live = false;
        self.release_for_reuse(&[process], false);
        if let Some(parent) = self.process_state.activations[process].parent {
            let parent = &mut self.process_state.activations[parent];
            if let Some((join, children)) = &mut parent.join {
                let member = children.contains(&process);
                children.retain(|&child| child != process);
                if member && (*join == ForkJoin::Any || children.is_empty()) {
                    parent.join = None;
                    let id = self.process_state.activations[process].parent.unwrap();
                    let state = &mut self.process_state;
                    if state.activations[id].live
                        && state.activations[id].control.suspended.is_none()
                    {
                        wake_in_region(
                            &self.model.processes,
                            &mut state.ready,
                            &mut state.reactive,
                            id,
                        );
                    }
                }
            }
        }
    }

    pub(super) fn retire_activation(&mut self, process: usize) {
        if self
            .process_state
            .activations
            .get(process)
            .is_some_and(|activation| !activation.live)
        {
            self.cancel_semaphore_wait(process);
            self.cancel_mailbox_wait(process);
            self.process_state.call_stacks.remove(&process);
        }
        let mut next = Some(process);
        while let Some(id) = next {
            let has_children = self
                .activation_children
                .get(id)
                .is_some_and(|&count| count > 0);
            let Some(activation) = self.process_state.activations.get_mut(id) else {
                break;
            };
            if activation.live {
                break;
            }
            if activation.control.exposed
                && let Some(frame) = &self.model.processes[id].frame
                && let Some(value) = activation.private.value_of(frame.random)
            {
                activation.control.random =
                    Some(value.0.to_u64_digits().first().copied().unwrap_or(0));
            }
            activation.private.clear();
            activation.locals.clear();
            if has_children {
                break;
            }
            next = activation.parent;
            self.set_parent(id, None);
        }
    }

    fn collect_activation_cells(&mut self) {
        let state = &mut self.process_state;
        let mut mapping = vec![None; state.cells.len()];
        let mut cells = Vec::new();
        for activation in &mut state.activations {
            for cell in activation.locals.iter_mut().flatten() {
                let new = *mapping[*cell].get_or_insert_with(|| {
                    cells.push(state.cells[*cell].clone());
                    cells.len() - 1
                });
                *cell = new;
            }
        }
        state.cells = cells;
    }

    /// Keep heap handles stored outside the backend memory alive at host safepoints.
    pub fn retain_activation_roots<E: super::SimulationExecutor<Backend = B>>(
        &self,
        executor: &mut E,
    ) {
        let mut values = Vec::new();
        for (id, activation) in self.process_state.activations.iter().enumerate() {
            if !activation.live {
                continue;
            }
            if let Some(frame) = &self.model.processes[id].frame {
                values.extend(
                    frame
                        .private
                        .iter()
                        .copied()
                        .zip(activation.private.values()),
                );
                values.extend(frame.locals.iter().zip(&activation.locals).filter_map(
                    |(&slot, cell)| cell.map(|cell| (slot, self.process_state.cells[cell].clone())),
                ));
            }
        }
        executor.retain_process_values(&values);
        if !self.process_state.call_stacks.is_empty() {
            let stacks: Vec<_> = self
                .process_state
                .call_stacks
                .iter()
                .map(|(&process, saved)| {
                    let kernel = self
                        .process_state
                        .activations
                        .get(process)
                        .map_or(process, |activation| activation.kernel);
                    (kernel, saved)
                })
                .collect();
            executor.retain_call_stacks(&stacks);
        }
    }

    /// Whether spawned activation `id` may be reused for a new child: it
    /// ended, no process handle refers to it (IEEE 1800-2023 9.7), and no
    /// activation names it as its parent.
    fn activation_reusable(&self, id: usize) -> bool {
        self.process_state
            .activations
            .get(id)
            .is_some_and(|activation| {
                !activation.live
                    && !activation.control.exposed
                    // A killed compiler-owned activation may still be
                    // named by its armed variable, which its trailing
                    // disarm never ran to clear: reusing its slot could
                    // let that stale handle kill an unrelated process.
                    // One that finished normally disarmed itself first.
                    && !(activation.control.internal && activation.control.killed)
            })
            && self.activation_children.get(id) == Some(&0)
    }

    /// Clear every registration that names the ended activations `ids`, and
    /// offer each that may be reused for a new child: the only way an
    /// activation reaches [`Self::take_reusable_activation`], so that a
    /// reused one inherits nothing of the one before. `killed` when they
    /// ended before running to their end (`disable`, `$exit`), which may
    /// leave them queued or waiting anywhere.
    pub(super) fn release_for_reuse(&mut self, ids: &[usize], killed: bool) {
        if killed {
            let released: fxhash::FxHashSet<usize> = ids.iter().copied().collect();
            self.process_wakeups
                .retain(|Reverse((_, id))| !released.contains(id));
            self.spawned.retain(|id| !released.contains(id));
        }
        for &id in ids {
            if self
                .process_state
                .activations
                .get(id)
                .is_none_or(|activation| activation.live)
            {
                continue;
            }
            let state = &mut self.process_state;
            state.waiters.remove(id);
            state.ready.remove(&id);
            state.deferred_ready.remove(&id);
            state.inactive.remove(&id);
            state.reactive.remove(&id);
            state.observed.remove(&id);
            state.re_inactive.remove(&id);
            state.call_stacks.remove(&id);
            // The named events it waits for, or observes.
            let control = &mut state.activations[id].control;
            let events: Vec<u32> = std::mem::take(&mut control.events)
                .into_iter()
                .chain(std::mem::take(&mut control.observed_events))
                .collect();
            for handle in events {
                if let Some(event) = state.events.get_mut(&handle) {
                    event.waiters.retain(|&waiter| waiter != id);
                }
            }
            if self.continued == Some(id) {
                self.continued = None;
            }
            self.slot_resumed.retain(|&resumed| resumed != id);
            self.cancel_semaphore_wait(id);
            self.cancel_mailbox_wait(id);
            if id >= self.kernel_count && self.activation_reusable(id) {
                self.reusable_activations.push(Reverse(id));
            }
        }
    }

    /// Offer activation `id` for reuse if it may be, now that no process
    /// handle names it: its registrations were cleared when it ended (see
    /// [`Self::release_for_reuse`]).
    pub(super) fn offer_ended_activation(&mut self, id: usize) {
        if id >= self.kernel_count && self.activation_reusable(id) {
            self.reusable_activations.push(Reverse(id));
        }
    }

    /// The smallest reusable activation, if any. Every activation that is
    /// reusable was offered when it became so; an offered one that no longer
    /// is, or was taken already, is dropped.
    fn take_reusable_activation(&mut self) -> Option<usize> {
        while let Some(Reverse(id)) = self.reusable_activations.pop() {
            if self.activation_reusable(id) {
                // A recount of every link costs a scan of the activations:
                // check the counts at every power of two reuses.
                self.reuses += 1;
                debug_assert!(
                    !self.reuses.is_power_of_two()
                        || self.activation_children == self.count_children(),
                    "the counts of children follow the parent links"
                );
                return Some(id);
            }
        }
        None
    }

    /// Link activation `id` to `parent`, or unlink it. The only place that
    /// changes a link once activations are counted, so each activation's
    /// count of children stays the number of links to it: unlinking the
    /// last child of an ended activation offers it for reuse.
    fn set_parent(&mut self, id: usize, parent: Option<usize>) {
        let previous = std::mem::replace(&mut self.process_state.activations[id].parent, parent);
        if previous == parent {
            return;
        }
        if let Some(previous) = previous {
            let count = &mut self.activation_children[previous];
            *count -= 1;
            if *count == 0 {
                self.release_for_reuse(&[previous], false);
            }
        }
        if let Some(parent) = parent {
            self.activation_children[parent] += 1;
        }
    }

    /// The number of activations linked to each activation as their parent.
    fn count_children(&self) -> Vec<u32> {
        let activations = &self.process_state.activations;
        let mut children = vec![0; activations.len()];
        for activation in activations {
            if let Some(count) = activation
                .parent
                .and_then(|parent| children.get_mut(parent))
            {
                *count += 1;
            }
        }
        children
    }

    /// Recount the children of the activations and offer every reusable one,
    /// after the activations were replaced by those of a saved state, where
    /// each ended one was released (see [`Self::release_for_reuse`]) before.
    pub(super) fn rebuild_activation_reuse(&mut self) {
        self.activation_children = self.count_children();
        self.reusable_activations.clear();
        for id in self.kernel_count..self.process_state.activations.len() {
            if self.activation_reusable(id) {
                self.reusable_activations.push(Reverse(id));
            }
        }
    }

    pub(super) fn rebuild_activation_models(&mut self) {
        self.model.processes.truncate(self.kernel_count);
        for index in 0..self.process_state.activations.len() {
            let kernel = self.process_state.activations[index].kernel;
            if index == self.model.processes.len() {
                self.model
                    .processes
                    .push(self.model.processes[kernel].clone());
            }
        }
        self.rebuild_activation_reuse();
    }
}

/// Check, in a debug build, that `image` holds the values that reading
/// each of `slots` from the state memory of `backend` gives. An activation
/// that has not run keeps no image.
fn debug_assert_frame<B: SimBackend>(backend: &B, slots: &[SignalRef], image: &FrameImage) {
    if cfg!(debug_assertions) && !image.is_empty() {
        for (index, &slot) in slots.iter().enumerate() {
            assert_eq!(
                image.get(index),
                Some(read(backend, slot)),
                "private slot {index} of a frame image"
            );
        }
    }
}

/// The value of `signal` in the frame of activation `process`: a cell of
/// one of its locals, or one of its private slots, decoded.
pub(super) fn activation_value<'a>(
    activations: &'a [Activation],
    cells: &'a [Value],
    refs: &crate::timing::ProcessRefs,
    process: usize,
    signal: SignalRef,
) -> Option<std::borrow::Cow<'a, Value>> {
    let frame = refs.frame.as_ref()?;
    let activation = activations.get(process)?;
    if let Some(index) = frame.locals.iter().position(|&slot| slot == signal) {
        return activation
            .locals
            .get(index)?
            .map(|cell| std::borrow::Cow::Borrowed(&cells[cell]));
    }
    activation
        .private
        .value_of(signal)
        .map(std::borrow::Cow::Owned)
}

/// Evaluate one pure dispatch entry without exposing another activation's
/// frame or committing any probe temporary back into its persistent state.
pub(super) fn event_probe<B: SimBackend>(
    backend: &mut B,
    refs: &crate::timing::ProcessRefs,
    activation: &Activation,
    cells: &[Value],
    probe: &celox_design::ProcessEventProbe<SignalRef>,
) -> Result<Value, Halt> {
    let frame = refs
        .frame
        .as_ref()
        .ok_or(SimulatorErrorCode::InternalError)?;
    // What the probe may overwrite: the private slots of the frame, as one
    // image, and its other slots.
    let layout = activation.private.layout().cloned().unwrap_or_else(|| {
        Arc::new(ImageLayout::new(
            &frame.private,
            backend.layout().four_state,
        ))
    });
    let saved_private = FrameImage::capture(&layout, backend);
    let mut seen = fxhash::FxHashSet::default();
    let mut saved = Vec::new();
    for &slot in frame.locals.iter().chain(&probe.scratch).chain(&[
        refs.resume,
        refs.status,
        refs.delay,
        probe.result,
    ]) {
        if layout.index_of(slot).is_none() && seen.insert(slot) {
            saved.push((slot, read(backend, slot)));
        }
    }
    activation.private.restore(backend);
    for (&slot, cell) in frame.locals.iter().zip(&activation.locals) {
        if let Some(cell) = cell {
            write(backend, slot, &cells[*cell]);
        }
    }
    backend.set(refs.resume, probe.resume);
    let result = backend
        .run_process(activation.kernel)
        .map_err(Halt::from)
        .and_then(|()| {
            let status: u8 = backend.get_as(refs.status);
            if status != celox_design::ProcessStatus::EventProbe.code() {
                return Err(SimulatorErrorCode::Runtime {
                    message: "event-expression probe did not return through its pure entry".into(),
                    signals: Vec::new(),
                }
                .into());
            }
            Ok(read(backend, probe.result))
        });
    saved_private.restore(backend);
    for (slot, value) in saved {
        write(backend, slot, &value);
    }
    result
}

#[cfg(test)]
mod tests;

/// The run-time error of a function called by a concurrent assertion's
/// action that forks a background process: neither the assertion's process
/// nor the final procedure that fails the pending attempts of a strong until
/// property is an initial or always procedure (IEEE 1800-2023 13.4.4). The
/// analyzer rejects the other forks of these processes.
pub(super) fn assertion_action_fork() -> Halt {
    SimulatorErrorCode::Runtime {
        message: "background process scheduled by a function called from a concurrent \
                  assertion action; IEEE 1800-2023 13.4.4 requires an initial/always process"
            .into(),
        signals: Vec::new(),
    }
    .into()
}
