//! Fine-grain process control over dynamic activations (IEEE 1800-2023 9.7).
use super::{SimulationExecutor, SimulationState, SuspendedProcess, wait::wake_in_region};
use crate::{SignalRef, SimulatorErrorCode, backend::SimBackend, timing::Halt};
use celox_design::{HostRequestSite, PROCESS_LEVEL_WAIT, ProcessOp};

fn error(message: impl Into<String>) -> Halt {
    SimulatorErrorCode::Runtime {
        message: message.into(),
        signals: Vec::new(),
    }
    .into()
}

impl<B: SimBackend> SimulationState<B> {
    fn process_slot(&self, backend: &B, current: usize, target: usize, slot: SignalRef) -> u64 {
        if current == target {
            return backend.get_as(slot);
        }
        let activation = &self.process_state.activations[target];
        let frame = self.model.processes[target].frame.as_ref().unwrap();
        if activation.private.is_empty() {
            return activation.control.random.unwrap_or(0);
        }
        let index = frame.private.iter().position(|s| *s == slot).unwrap();
        activation.private[index]
            .0
            .to_u64_digits()
            .first()
            .copied()
            .unwrap_or(0)
    }
    fn set_process_slot(
        &mut self,
        backend: &mut B,
        current: usize,
        target: usize,
        slot: SignalRef,
        value: u64,
    ) {
        if current == target {
            backend.set(slot, value);
            return;
        }
        let frame = self.model.processes[target].frame.as_ref().unwrap();
        if self.process_state.activations[target].private.is_empty() {
            self.process_state.activations[target].control.random = Some(value);
            return;
        }
        let index = frame.private.iter().position(|s| *s == slot).unwrap();
        self.process_state.activations[target].private[index] = (value.into(), 0u8.into());
    }
    fn wake_controlled(&mut self, process: usize) {
        if !self.process_state.activations[process].live
            || self.process_state.activations[process]
                .control
                .suspended
                .is_some()
        {
            return;
        }
        self.process_state.waiters.remove(process);
        wake_in_region(
            &self.model.processes,
            &mut self.process_state.ready,
            &mut self.process_state.reactive,
            process,
        );
    }
    pub(super) fn process_changed(&mut self, target: usize) {
        // Only self() can expose an identity to status predicates or await().
        // Ordinary fork workloads need no process-observer scan.
        if !self
            .process_state
            .activations
            .get(target)
            .is_some_and(|a| a.control.exposed)
        {
            return;
        }
        let completed = self
            .process_state
            .activations
            .get(target)
            .is_some_and(|a| !a.live);
        let mut wake = Vec::new();
        for (id, activation) in self.process_state.activations.iter_mut().enumerate() {
            if !activation.live || id == target {
                continue;
            }
            if completed && activation.control.awaiting == Some(target) {
                activation.control.awaiting = None;
                wake.push(id);
            }
            if activation.control.level_wait && activation.control.observes.contains(&target) {
                wake.push(id);
            }
        }
        for id in wake {
            self.wake_controlled(id);
        }
    }
    fn detach_process(
        &mut self,
        current: usize,
        target: usize,
        now: u64,
        backend: &B,
    ) -> SuspendedProcess {
        let state = &self.process_state;
        let runnable =
            target == current || state.ready.contains(&target) || state.reactive.contains(&target);
        let wake_time = self
            .process_wakeups
            .iter()
            .find_map(|std::cmp::Reverse((time, id))| (*id == target).then_some(*time))
            .or_else(|| {
                (state.inactive.contains(&target) || state.re_inactive.contains(&target))
                    .then_some(now)
            });
        let wait_word = state.waiters.get(target).map(|_| {
            self.process_slot(backend, current, target, self.model.processes[target].delay)
        });
        self.process_state.ready.remove(&target);
        self.process_state.reactive.remove(&target);
        self.process_state.inactive.remove(&target);
        self.process_state.re_inactive.remove(&target);
        self.process_state.waiters.remove(target);
        self.process_wakeups
            .retain(|std::cmp::Reverse((_, id))| *id != target);
        SuspendedProcess {
            runnable,
            wake_time,
            wait_word,
        }
    }
    pub(super) fn process_request<E: SimulationExecutor<Backend = B>>(
        &mut self,
        executor: &mut E,
        now: u64,
        current: usize,
        request: &HostRequestSite<SignalRef>,
    ) -> Result<bool, Halt> {
        let op = ProcessOp::from_code(request.operation & 0xff)
            .ok_or(SimulatorErrorCode::InternalError)?;
        if matches!(
            op,
            ProcessOp::Trigger
                | ProcessOp::TriggerNonblocking
                | ProcessOp::WaitEvent
                | ProcessOp::Triggered
                | ProcessOp::WaitTriggered
        ) {
            return self.event_request(executor, now, current, op, request);
        }
        if matches!(
            op,
            ProcessOp::SemaphoreNew
                | ProcessOp::SemaphorePut
                | ProcessOp::SemaphoreGet
                | ProcessOp::SemaphoreTryGet
        ) {
            return self.semaphore_request(executor, current, op, request);
        }
        if matches!(
            op,
            ProcessOp::MailboxNew
                | ProcessOp::MailboxNum
                | ProcessOp::MailboxPut
                | ProcessOp::MailboxTryPut
                | ProcessOp::MailboxGet
                | ProcessOp::MailboxTryGet
                | ProcessOp::MailboxPeek
                | ProcessOp::MailboxTryPeek
        ) {
            return self.mailbox_request(executor, current, op, request);
        }
        let count = match op {
            ProcessOp::SelfHandle | ProcessOp::Observe => 0,
            ProcessOp::Srandom | ProcessOp::SetRandstate => 2,
            _ => 1,
        };
        if request.args.len() != count || request.results.len() != 1 {
            return Err(SimulatorErrorCode::InternalError.into());
        }
        if op == ProcessOp::Observe {
            let control = &mut self.process_state.activations[current].control;
            control.observes.clear();
            control.observed_events.clear();
            return Ok(true);
        }
        let final_identity = self.finalizing.and_then(|_| self.final_process_identity());
        let target = if op == ProcessOp::SelfHandle {
            final_identity.unwrap_or(current)
        } else {
            let raw: u32 = executor.backend().get_as(request.args[0].id);
            let target = raw
                .checked_sub(1)
                .map(|id| id as usize)
                .ok_or_else(|| error("method called on a null process handle"))?;
            if !self
                .process_state
                .activations
                .get(target)
                .is_some_and(|a| a.control.exposed)
            {
                return Err(error("invalid process handle"));
            }
            target
        };
        if matches!(
            op,
            ProcessOp::Kill | ProcessOp::Await | ProcessOp::Suspend | ProcessOp::Resume
        ) && self.model.processes[target].kind != celox_design::ProcessKind::Regular
        {
            return Err(error(
                "process control requires an initial, always, or fork process",
            ));
        }
        let mut value = 0u32;
        match op {
            ProcessOp::SelfHandle => {
                value = u32::try_from(target)
                    .ok()
                    .and_then(|id| id.checked_add(1))
                    .ok_or_else(|| error("process handle space exhausted"))?;
                self.process_state.activations[target].control.exposed = true;
            }
            ProcessOp::Status => {
                let activation = &self.process_state.activations[target];
                value = if activation.control.killed {
                    4
                } else if !activation.live {
                    0
                } else if activation.control.suspended.is_some() {
                    3
                } else if target == current
                    || Some(target) == final_identity
                    || self.process_state.ready.contains(&target)
                    || self.process_state.reactive.contains(&target)
                {
                    1
                } else {
                    2
                };
                if request.operation & 0x100 != 0
                    && !self.process_state.activations[current]
                        .control
                        .observes
                        .contains(&target)
                {
                    self.process_state.activations[current]
                        .control
                        .observes
                        .push(target);
                }
            }
            ProcessOp::Kill => {
                // Killing an ancestor retires the caller too. Capture the live
                // backend frame before descendant cancellation can release it.
                self.save_activation(executor.backend(), current);
                let mut program_ended = self.model.processes[target].program.is_some()
                    && self.process_state.activations[target].live;
                for id in self.disable_children(target) {
                    program_ended |= self.model.processes[id].program.is_some();
                    executor.cancel_deferred_assertion_process(id);
                }
                if self.process_state.activations[target].live {
                    self.detach_process(current, target, now, executor.backend());
                    self.process_state.activations[target].control.killed = true;
                    self.process_state.activations[target].control.suspended = None;
                    self.process_state.activations[target].control.awaiting = None;
                    self.end_activation(target);
                    executor.cancel_deferred_assertion_process(target);
                    self.process_changed(target);
                    // The current frame is saved and retired by run_process.
                    // A cancelled external target will never run again.
                    if target != current {
                        self.retire_activation(target);
                    }
                }
                if program_ended && self.finalizing.is_none() {
                    self.program_kernel_ended(executor.backend())?;
                }
            }
            ProcessOp::Await => {
                if target == current {
                    return Err(error("a process cannot await itself"));
                }
                if self.process_state.activations[target].live {
                    self.process_state.activations[current].control.awaiting = Some(target);
                    return Ok(false);
                }
            }
            ProcessOp::Suspend => {
                if target == current && request.operation & 0x200 != 0 {
                    return Err(error("a function cannot suspend its current process"));
                }
                if self.process_state.activations[target].live
                    && self.process_state.activations[target]
                        .control
                        .suspended
                        .is_none()
                {
                    let saved = self.detach_process(current, target, now, executor.backend());
                    self.process_state.activations[target].control.suspended = Some(saved);
                    self.process_changed(target);
                }
            }
            ProcessOp::Resume => {
                if self.process_state.activations[target].live
                    && let Some(saved) = self.process_state.activations[target]
                        .control
                        .suspended
                        .take()
                {
                    if let Some(time) = saved.wake_time {
                        if time <= now {
                            self.wake_controlled(target);
                        } else {
                            self.process_wakeups.push(std::cmp::Reverse((time, target)));
                        }
                    } else if let Some(word) = saved.wait_word {
                        if word & PROCESS_LEVEL_WAIT != 0 {
                            self.wake_controlled(target);
                        } else {
                            self.save_activation(executor.backend(), current);
                            self.restore_activation(executor.backend_mut(), target);
                            let result =
                                self.rearm_waiter(executor.backend_mut(), target, word as usize);
                            self.restore_activation(executor.backend_mut(), current);
                            result?;
                        }
                    } else if saved.runnable
                        || (self.process_state.activations[target].join.is_none()
                            && self.process_state.activations[target]
                                .control
                                .awaiting
                                .is_none()
                            && self.process_state.activations[target]
                                .control
                                .events
                                .is_empty()
                            && self.process_state.activations[target]
                                .control
                                .semaphore
                                .is_none()
                            && (self.process_state.activations[target]
                                .control
                                .mailbox
                                .is_none()
                                || self.mailbox_served(target)))
                    {
                        self.wake_controlled(target);
                    }
                    self.process_changed(target);
                }
            }
            ProcessOp::Srandom | ProcessOp::GetRandstate | ProcessOp::SetRandstate => {
                // All final kernels execute on the same process RNG. Their
                // active frame is synchronized at each terminal continuation.
                let target = if Some(target) == final_identity {
                    current
                } else {
                    target
                };
                let frame = self.model.processes[target]
                    .frame
                    .as_ref()
                    .ok_or(SimulatorErrorCode::InternalError)?;
                let slot = frame.random;
                let mut state = self.process_slot(executor.backend(), current, target, slot);
                if state == 0 {
                    state = u64::from(frame.seed) | (1 << 32);
                }
                match op {
                    ProcessOp::Srandom => {
                        let seed: u32 = executor.backend().get_as(request.args[1].id);
                        self.set_process_slot(
                            executor.backend_mut(),
                            current,
                            target,
                            slot,
                            u64::from(seed) | (1 << 32),
                        );
                    }
                    ProcessOp::GetRandstate => {
                        value = executor
                            .backend_mut()
                            .heap_mut()
                            .intern(format!("celox-process-rng:{state:09x}").as_bytes());
                    }
                    ProcessOp::SetRandstate => {
                        let handle: u32 = executor.backend().get_as(request.args[1].id);
                        let bytes = executor.backend().heap().string(handle);
                        let state = std::str::from_utf8(bytes)
                            .ok()
                            .and_then(|s| s.strip_prefix("celox-process-rng:"))
                            .and_then(|s| u64::from_str_radix(s, 16).ok())
                            .filter(|&n| n <= 0x1ffffffff && n & (1 << 32) != 0)
                            .ok_or_else(|| error("invalid process random state"))?;
                        self.set_process_slot(executor.backend_mut(), current, target, slot, state);
                    }
                    _ => unreachable!(),
                }
            }
            ProcessOp::Observe
            | ProcessOp::Trigger
            | ProcessOp::TriggerNonblocking
            | ProcessOp::WaitEvent
            | ProcessOp::Triggered
            | ProcessOp::WaitTriggered
            | ProcessOp::SemaphoreNew
            | ProcessOp::SemaphorePut
            | ProcessOp::SemaphoreGet
            | ProcessOp::SemaphoreTryGet
            | ProcessOp::MailboxNew
            | ProcessOp::MailboxNum
            | ProcessOp::MailboxPut
            | ProcessOp::MailboxTryPut
            | ProcessOp::MailboxGet
            | ProcessOp::MailboxTryGet
            | ProcessOp::MailboxPeek
            | ProcessOp::MailboxTryPeek => unreachable!(),
        }
        if let Some(result) = request.results.first() {
            executor.backend_mut().set(result.id, value);
        }
        Ok(self.process_state.activations[current].live
            && self.process_state.activations[current]
                .control
                .suspended
                .is_none())
    }

    /// Operations on named events (IEEE 1800-2023 15.5). A handle of 0 is
    /// `null`: triggering it has no effect and it is never triggered, and a
    /// process waiting only for it blocks forever (15.5.5.2).
    fn event_request<E: SimulationExecutor<Backend = B>>(
        &mut self,
        executor: &mut E,
        now: u64,
        current: usize,
        op: ProcessOp,
        request: &HostRequestSite<SignalRef>,
    ) -> Result<bool, Halt> {
        let valid = match op {
            ProcessOp::WaitEvent => !request.args.is_empty(),
            _ => request.args.len() == 1,
        };
        if !valid || request.results.len() != 1 {
            return Err(SimulatorErrorCode::InternalError.into());
        }
        let handles: Vec<u32> = request
            .args
            .iter()
            .map(|arg| executor.backend().get_as(arg.id))
            .collect();
        self.expire_events(now);
        let triggered = |state: &crate::timing::ProcessState, handle: u32| {
            handle != 0
                && state
                    .events
                    .get(&handle)
                    .is_some_and(|event| event.triggered == Some(now))
        };
        let mut value = 0u32;
        let blocked = match op {
            ProcessOp::Trigger => {
                self.trigger_event(handles[0], now);
                false
            }
            ProcessOp::TriggerNonblocking => {
                if handles[0] != 0 {
                    self.process_state.nba_events.push(handles[0]);
                }
                false
            }
            ProcessOp::Triggered => {
                value = u32::from(triggered(&self.process_state, handles[0]));
                // A level-wait predicate checks it again at the next trigger.
                let control = &mut self.process_state.activations[current].control;
                if request.operation & 0x100 != 0
                    && handles[0] != 0
                    && !control.observed_events.contains(&handles[0])
                {
                    control.observed_events.push(handles[0]);
                    self.process_state
                        .events
                        .entry(handles[0])
                        .or_default()
                        .waiters
                        .push(current);
                }
                false
            }
            ProcessOp::WaitTriggered if triggered(&self.process_state, handles[0]) => false,
            ProcessOp::WaitEvent | ProcessOp::WaitTriggered => {
                let mut events: Vec<u32> = handles.into_iter().filter(|&h| h != 0).collect();
                for &handle in &events {
                    self.process_state
                        .events
                        .entry(handle)
                        .or_default()
                        .waiters
                        .push(current);
                }
                // A wait for only null events blocks forever; 0 is never
                // triggered.
                if events.is_empty() {
                    events.push(0);
                }
                self.process_state.activations[current].control.events = events;
                true
            }
            _ => unreachable!(),
        };
        if let Some(result) = request.results.first() {
            executor.backend_mut().set(result.id, value);
        }
        Ok(!blocked
            && self.process_state.activations[current].live
            && self.process_state.activations[current]
                .control
                .suspended
                .is_none())
    }

    /// Forget the events that were last triggered before time `now` and that
    /// no process waits for: their state is as if they were never used. Each
    /// time this runs at a later time it scans the events once.
    fn expire_events(&mut self, now: u64) {
        let state = &mut self.process_state;
        if state.event_time >= now {
            return;
        }
        state.event_time = now;
        let activations = &state.activations;
        state.events.retain(|&handle, event| {
            event.waiters.retain(|&process| {
                activations.get(process).is_some_and(|activation| {
                    activation.live
                        && (activation.control.events.contains(&handle)
                            || (activation.control.level_wait
                                && activation.control.observed_events.contains(&handle)))
                })
            });
            !event.waiters.is_empty() || event.triggered == Some(now)
        });
    }

    /// Trigger named event `handle` at time `now`: unblock the processes
    /// waiting for it (IEEE 1800-2023 15.5.1).
    pub(super) fn trigger_event(&mut self, handle: u32, now: u64) {
        if handle == 0 {
            return;
        }
        self.expire_events(now);
        let event = self.process_state.events.entry(handle).or_default();
        event.triggered = Some(now);
        let waiters = std::mem::take(&mut event.waiters);
        let mut suspended = Vec::new();
        for process in waiters {
            let Some(activation) = self.process_state.activations.get_mut(process) else {
                continue;
            };
            if !activation.live {
                continue;
            }
            // A suspended process misses the trigger: once resumed, it still
            // waits for a later one, as for a signal event (9.7).
            let blocked = activation.control.events.contains(&handle);
            let observed = activation.control.level_wait
                && activation.control.observed_events.contains(&handle);
            if (blocked || observed) && activation.control.suspended.is_some() {
                suspended.push(process);
                continue;
            }
            if observed {
                // The predicate observes the event again when it reruns.
                activation.control.observed_events.clear();
                self.wake_controlled(process);
                continue;
            }
            if !blocked {
                continue;
            }
            // A process waiting for several events stops waiting for all.
            for other in std::mem::take(&mut activation.control.events) {
                if other != handle
                    && let Some(event) = self.process_state.events.get_mut(&other)
                {
                    event.waiters.retain(|&waiter| waiter != process);
                }
            }
            self.wake_controlled(process);
        }
        if !suspended.is_empty()
            && let Some(event) = self.process_state.events.get_mut(&handle)
        {
            event.waiters.extend(suspended);
        }
    }

    /// Operations on semaphores (IEEE 1800-2023 15.3).
    fn semaphore_request<E: SimulationExecutor<Backend = B>>(
        &mut self,
        executor: &mut E,
        current: usize,
        op: ProcessOp,
        request: &HostRequestSite<SignalRef>,
    ) -> Result<bool, Halt> {
        let count = if op == ProcessOp::SemaphoreNew { 1 } else { 2 };
        if request.args.len() != count || request.results.len() != 1 {
            return Err(SimulatorErrorCode::InternalError.into());
        }
        let args: Vec<u32> = request
            .args
            .iter()
            .map(|arg| executor.backend().get_as(arg.id))
            .collect();
        let arg = |index: usize| args[index];
        let mut value = 0u32;
        let mut blocked = false;
        if op == ProcessOp::SemaphoreNew {
            let heap = executor.backend_mut().heap_mut();
            self.forget_collected_semaphores(heap);
            value = heap.new_object(SCHEDULER_CLASS, 0);
            let keys = i64::from(arg(0) as i32);
            self.process_state.semaphores.insert(
                value,
                crate::timing::Semaphore {
                    keys,
                    queue: Default::default(),
                },
            );
        } else {
            let handle = arg(0);
            let keys = arg(1) as i32;
            if keys < 0 {
                return Err(error(format!(
                    "semaphore {} with a negative key count",
                    match op {
                        ProcessOp::SemaphorePut => "put",
                        ProcessOp::SemaphoreGet => "get",
                        _ => "try_get",
                    }
                )));
            }
            let keys = keys as u32;
            if !self.process_state.semaphores.contains_key(&handle) {
                return Err(error(if handle == 0 {
                    "method called on a null semaphore handle"
                } else {
                    "invalid semaphore handle"
                }));
            }
            match op {
                ProcessOp::SemaphorePut => {
                    self.process_state.semaphores.get_mut(&handle).unwrap().keys += i64::from(keys);
                    self.serve_semaphore(handle);
                }
                ProcessOp::SemaphoreTryGet => {
                    let semaphore = self.process_state.semaphores.get_mut(&handle).unwrap();
                    if semaphore.keys >= i64::from(keys) {
                        semaphore.keys -= i64::from(keys);
                        value = 1;
                    }
                }
                ProcessOp::SemaphoreGet => {
                    // Available keys are taken at once; otherwise the process
                    // waits in arrival order (15.3.3).
                    let semaphore = self.process_state.semaphores.get_mut(&handle).unwrap();
                    if semaphore.keys >= i64::from(keys) {
                        semaphore.keys -= i64::from(keys);
                    } else {
                        // An entry of an earlier process with this ID is stale.
                        semaphore.queue.retain(|&(process, _)| process != current);
                        semaphore.queue.push_back((current, keys));
                        self.process_state.activations[current].control.semaphore = Some(handle);
                        blocked = true;
                    }
                }
                _ => unreachable!(),
            }
        }
        if let Some(result) = request.results.first() {
            executor.backend_mut().set(result.id, value);
        }
        Ok(!blocked
            && self.process_state.activations[current].live
            && self.process_state.activations[current]
                .control
                .suspended
                .is_none())
    }

    /// Forget the semaphores the heap collected that no process waits for,
    /// once there are twice as many as the last time this looked.
    fn forget_collected_semaphores(&mut self, heap: &celox_heap::Heap) {
        let state = &mut self.process_state;
        if state.semaphores.len() < state.semaphore_limit {
            return;
        }
        state.semaphores.retain(|&handle, semaphore| {
            heap.contains_object(handle) || !semaphore.queue.is_empty()
        });
        state.semaphore_limit = (2 * state.semaphores.len()).max(64);
    }

    /// Forget the mailboxes the heap collected that no process waits for,
    /// once there are twice as many as the last time this looked.
    fn forget_collected_mailboxes(&mut self, heap: &celox_heap::Heap) {
        let state = &mut self.process_state;
        if state.mailboxes.len() < state.mailbox_limit {
            return;
        }
        state
            .mailboxes
            .retain(|&handle, mailbox| heap.contains_object(handle) || !mailbox.is_idle());
        state.mailbox_limit = (2 * state.mailboxes.len()).max(64);
    }

    /// Stop process `process`, which ended while it waited for a mailbox,
    /// from waiting: its turn passes to the next process.
    pub(super) fn cancel_mailbox_wait(&mut self, process: usize) {
        let Some(handle) = self.process_state.activations[process]
            .control
            .mailbox
            .take()
        else {
            return;
        };
        if let Some(mailbox) = self.process_state.mailboxes.get_mut(&handle) {
            mailbox.forget(process);
        }
    }

    /// Stop process `process`, which ended without its keys, from waiting
    /// for a semaphore: the processes behind it may get keys now.
    pub(super) fn cancel_semaphore_wait(&mut self, process: usize) {
        let Some(handle) = self.process_state.activations[process]
            .control
            .semaphore
            .take()
        else {
            return;
        };
        if let Some(semaphore) = self.process_state.semaphores.get_mut(&handle) {
            semaphore.queue.retain(|&(waiter, _)| waiter != process);
            self.serve_semaphore(handle);
        }
    }

    /// Give the keys of semaphore `handle` to its waiting processes in
    /// arrival order, while the first one's request can be met (15.3.2,
    /// 15.3.3).
    fn serve_semaphore(&mut self, handle: u32) {
        loop {
            let Some(&(process, keys)) = self.process_state.semaphores[&handle].queue.front()
            else {
                return;
            };
            let waiting = self
                .process_state
                .activations
                .get(process)
                .is_some_and(|activation| {
                    activation.live && activation.control.semaphore == Some(handle)
                });
            let semaphore = self.process_state.semaphores.get_mut(&handle).unwrap();
            if waiting {
                if semaphore.keys < i64::from(keys) {
                    return;
                }
                semaphore.keys -= i64::from(keys);
            }
            semaphore.queue.pop_front();
            if waiting {
                self.process_state.activations[process].control.semaphore = None;
                self.wake_controlled(process);
            }
        }
    }

    /// Operations on mailboxes (IEEE 1800-2023 15.4). A mailbox is a heap
    /// object whose handle fields hold its queue of messages and the
    /// messages it handed over or holds back (see [`MailboxQueue`]). A
    /// blocking operation that cannot complete returns 0 and parks the
    /// process; the operation that serves it does its work and wakes it, and
    /// its call again collects the result.
    fn mailbox_request<E: SimulationExecutor<Backend = B>>(
        &mut self,
        executor: &mut E,
        current: usize,
        op: ProcessOp,
        request: &HostRequestSite<SignalRef>,
    ) -> Result<bool, Halt> {
        let (count, results) = match op {
            ProcessOp::MailboxNew | ProcessOp::MailboxPut | ProcessOp::MailboxTryPut => (2, 1),
            ProcessOp::MailboxNum => (1, 1),
            _ => (1, 2),
        };
        if request.args.len() != count || request.results.len() != results {
            return Err(SimulatorErrorCode::InternalError.into());
        }
        let backend = executor.backend();
        let handle: u32 = backend.get_as(request.args[0].id);
        if op == ProcessOp::MailboxNew {
            let bound = handle as i32;
            if bound < 0 {
                return Err(error("mailbox with a negative bound"));
            }
            let layout: u64 = backend.get_as(request.args[1].id);
            let heap = executor.backend_mut().heap_mut();
            self.forget_collected_mailboxes(heap);
            let mailbox = heap.new_object(SCHEDULER_CLASS, 3 * celox_heap::HANDLE_BYTES);
            for queue in [
                MailboxQueue::Messages,
                MailboxQueue::Handed,
                MailboxQueue::Held,
            ] {
                heap.declare_handle(
                    mailbox,
                    queue.offset() as u32,
                    celox_heap::HandleKind::Object,
                );
            }
            self.process_state.mailboxes.insert(
                mailbox,
                crate::timing::Mailbox {
                    bound: bound as u32,
                    layout,
                    ..Default::default()
                },
            );
            executor.backend_mut().set(request.results[0].id, mailbox);
            return Ok(true);
        }
        let Some(mailbox) = self.process_state.mailboxes.get_mut(&handle) else {
            return Err(error(if handle == 0 {
                "method called on a null mailbox handle"
            } else {
                "invalid mailbox handle"
            }));
        };
        let (bound, layout) = (mailbox.bound, mailbox.layout);
        let message = if matches!(op, ProcessOp::MailboxPut | ProcessOp::MailboxTryPut) {
            let (value, mask) = executor.backend().get_four_state(request.args[1].id);
            let word =
                |value: num_bigint::BigUint| value.to_u64_digits().first().copied().unwrap_or(0);
            Some((word(value), word(mask)))
        } else {
            None
        };
        let mut queues = MailboxQueues {
            heap: executor.backend_mut().heap_mut(),
            handle,
            layout,
        };
        queues.drop_ended(self.process_state.mailboxes.get_mut(&handle).unwrap());
        let mut status = 0u32;
        let mut received = (0u64, 0u64);
        let mut blocked = false;
        let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
        match op {
            ProcessOp::MailboxNum => status = queues.len(MailboxQueue::Messages) as u32,
            ProcessOp::MailboxPut | ProcessOp::MailboxTryPut => {
                if let Some(index) = mailbox.delivered.iter().position(|&p| p == current) {
                    // A `get` made room and moved the message in.
                    mailbox.delivered.swap_remove(index);
                    status = 1;
                } else if bound == 0 || queues.len(MailboxQueue::Messages) < u64::from(bound) {
                    queues.push(MailboxQueue::Messages, message.unwrap());
                    status = 1;
                    self.serve_mailbox(&mut queues);
                } else if op == ProcessOp::MailboxPut {
                    mailbox.forget(current);
                    mailbox.putters.push_back(current);
                    queues.push(MailboxQueue::Held, message.unwrap());
                    blocked = true;
                }
            }
            _ => {
                let get = matches!(op, ProcessOp::MailboxGet | ProcessOp::MailboxTryGet);
                if let Some(index) = mailbox.handed.iter().position(|&p| p == current) {
                    // A `put` handed the message over.
                    mailbox.handed.remove(index);
                    received = queues.take(MailboxQueue::Handed, index as u64);
                    status = 1;
                } else if queues.len(MailboxQueue::Messages) > 0 {
                    status = 1;
                    if get {
                        received = queues.take(MailboxQueue::Messages, 0);
                        self.serve_mailbox(&mut queues);
                    } else {
                        received = queues.read(MailboxQueue::Messages, 0);
                    }
                } else if matches!(op, ProcessOp::MailboxGet | ProcessOp::MailboxPeek) {
                    mailbox.forget(current);
                    if get {
                        mailbox.getters.push_back(current);
                    } else {
                        mailbox.peekers.push(current);
                    }
                    blocked = true;
                }
            }
        }
        // A process waits for one mailbox at a time, and until it collects
        // what a served operation left for it.
        self.process_state.activations[current].control.mailbox = blocked.then_some(handle);
        let backend = executor.backend_mut();
        backend.set(request.results[0].id, status);
        if let Some(result) = request.results.get(1) {
            backend.set_four_state(result.id, received.0.into(), received.1.into());
        }
        Ok(!blocked
            && self.process_state.activations[current].live
            && self.process_state.activations[current]
                .control
                .suspended
                .is_none())
    }

    /// Serve the processes waiting for mailbox `queues.handle`, in arrival
    /// order, while it has room and messages: the first process in `put`
    /// moves its message in (15.4.3), every process in `peek` gets a copy of
    /// the first message (15.4.7), and the first process in `get` takes it
    /// (15.4.5). Each served process is woken to collect the result.
    fn serve_mailbox(&mut self, queues: &mut MailboxQueues<'_>) {
        let handle = queues.handle;
        loop {
            let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
            let room =
                mailbox.bound == 0 || queues.len(MailboxQueue::Messages) < u64::from(mailbox.bound);
            if room && let Some(putter) = mailbox.putters.pop_front() {
                let message = queues.take(MailboxQueue::Held, 0);
                if self.mailbox_waiter(handle, putter) {
                    queues.push(MailboxQueue::Messages, message);
                    let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
                    mailbox.delivered.push(putter);
                    self.wake_controlled(putter);
                }
                continue;
            }
            if queues.len(MailboxQueue::Messages) == 0 {
                return;
            }
            let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
            let peekers = std::mem::take(&mut mailbox.peekers);
            if !peekers.is_empty() {
                let first = queues.read(MailboxQueue::Messages, 0);
                for peeker in peekers {
                    if self.mailbox_waiter(handle, peeker) {
                        queues.push(MailboxQueue::Handed, first);
                        let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
                        mailbox.handed.push(peeker);
                        self.wake_controlled(peeker);
                    }
                }
            }
            let getter = loop {
                let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
                match mailbox.getters.pop_front() {
                    Some(getter) if self.mailbox_waiter(handle, getter) => break Some(getter),
                    Some(_) => {}
                    None => break None,
                }
            };
            let Some(getter) = getter else {
                return;
            };
            let message = queues.take(MailboxQueue::Messages, 0);
            queues.push(MailboxQueue::Handed, message);
            let mailbox = self.process_state.mailboxes.get_mut(&handle).unwrap();
            mailbox.handed.push(getter);
            self.wake_controlled(getter);
        }
    }

    /// Whether the mailbox that `process` waits for served it, so that it
    /// only has to collect the result.
    fn mailbox_served(&self, process: usize) -> bool {
        self.process_state.activations[process]
            .control
            .mailbox
            .and_then(|handle| self.process_state.mailboxes.get(&handle))
            .is_some_and(|mailbox| {
                mailbox.handed.contains(&process) || mailbox.delivered.contains(&process)
            })
    }

    /// Whether `process` still waits for mailbox `handle`.
    fn mailbox_waiter(&self, handle: u32, process: usize) -> bool {
        self.process_state
            .activations
            .get(process)
            .is_some_and(|activation| activation.live && activation.control.mailbox == Some(handle))
    }
}

/// The heap queues of a mailbox object, one per handle field.
#[derive(Clone, Copy)]
enum MailboxQueue {
    /// Its messages, in put order.
    Messages,
    /// The messages taken or copied for the processes of
    /// [`crate::timing::Mailbox::handed`], in the same order.
    Handed,
    /// The messages of the processes of [`crate::timing::Mailbox::putters`],
    /// in the same order.
    Held,
}

impl MailboxQueue {
    fn offset(self) -> usize {
        self as usize * celox_heap::HANDLE_BYTES
    }
}

/// The queues of the heap object of mailbox `handle`.
struct MailboxQueues<'a> {
    heap: &'a mut celox_heap::Heap,
    handle: u32,
    layout: u64,
}

impl MailboxQueues<'_> {
    fn call(&mut self, op: u64, queue: u64, index: u64) -> u64 {
        self.heap
            .call(celox_heap::ops::encode(op, self.layout), queue, index, 0)
    }

    fn queue(&mut self, queue: MailboxQueue) -> u64 {
        self.heap
            .load(self.handle, queue.offset(), celox_heap::HANDLE_BYTES)
    }

    fn len(&mut self, queue: MailboxQueue) -> u64 {
        let handle = self.queue(queue);
        self.call(celox_heap::containers::LEN, handle, 0)
    }

    fn read(&mut self, queue: MailboxQueue, index: u64) -> (u64, u64) {
        use celox_heap::containers::{GET, GET_MASK};
        let handle = self.queue(queue);
        (
            self.call(GET, handle, index),
            self.call(GET_MASK, handle, index),
        )
    }

    fn push(&mut self, queue: MailboxQueue, (value, mask): (u64, u64)) {
        use celox_heap::containers::{INSERT, SET_MASK};
        let handle = self.queue(queue);
        let len = self.call(celox_heap::containers::LEN, handle, 0);
        let inserted = self.heap.call(
            celox_heap::ops::encode(INSERT, self.layout),
            handle,
            len,
            value,
        );
        let handle = self.heap.call(
            celox_heap::ops::encode(SET_MASK, self.layout),
            inserted,
            len,
            mask,
        );
        self.heap.store(
            self.handle,
            queue.offset(),
            celox_heap::HANDLE_BYTES,
            handle,
        );
    }

    fn take(&mut self, queue: MailboxQueue, index: u64) -> (u64, u64) {
        let message = self.read(queue, index);
        let handle = self.queue(queue);
        let handle = self.call(celox_heap::containers::DELETE, handle, index);
        self.heap.store(
            self.handle,
            queue.offset(),
            celox_heap::HANDLE_BYTES,
            handle,
        );
        message
    }

    /// Drop the entries of `mailbox` and their messages for processes that
    /// ended (see [`crate::timing::Mailbox::forget`]).
    fn drop_ended(&mut self, mailbox: &mut crate::timing::Mailbox) {
        use crate::timing::ENDED;
        for index in (0..mailbox.handed.len()).rev() {
            if mailbox.handed[index] == ENDED {
                mailbox.handed.remove(index);
                self.take(MailboxQueue::Handed, index as u64);
            }
        }
        for index in (0..mailbox.putters.len()).rev() {
            if mailbox.putters[index] == ENDED {
                mailbox.putters.remove(index);
                self.take(MailboxQueue::Held, index as u64);
            }
        }
    }
}

/// The class of the heap objects the scheduler owns, semaphores and
/// mailboxes: no class identifier, which hashes names to nonzero values, is
/// all ones.
const SCHEDULER_CLASS: u32 = u32::MAX;
