//! Checkpoints and state files of the scheduling state.

use std::{cmp::Reverse, collections::BinaryHeap};

use bit_set::BitSet;

use super::{ProcessWakeup, SimulationState};
use crate::{
    backend::{EventHandle, SimBackend},
    scheduler::{ClockDef, SimEvent},
    timing::ProcessState,
};

impl<B: SimBackend> SimulationState<B> {
    /// Capture the mutable scheduling state: time, pending events, clocks and
    /// the clock values edge detection compares against.
    pub fn snapshot(&self) -> SimulationSnapshot<B> {
        SimulationSnapshot {
            time: self.scheduler.time,
            clocks: self.scheduler.clocks.clone(),
            event_queue: self.scheduler.event_queue.clone(),
            last_clock_values: self.last_clock_values.clone(),
            last_clock_unknown: self.last_clock_unknown.clone(),
            process_wakeups: self.process_wakeups.clone(),
            process_state: self.process_state.clone(),
            declarations_pending: self.declarations_pending,
            host_pending: self.host_pending,
            finished: self.finished,
        }
    }

    /// Return to a state captured by [`Self::snapshot`] on a state built for
    /// the same design.
    ///
    /// Event handles may point into the compiled code of the instance that took
    /// the snapshot, so `remap` translates each pending event into a handle of
    /// this instance. If an event cannot be translated, nothing is changed and
    /// that event is returned.
    pub fn restore(
        &mut self,
        snapshot: &SimulationSnapshot<B>,
        mut remap: impl FnMut(B::Event) -> Option<B::Event>,
    ) -> Result<(), B::Event> {
        let event_queue = snapshot
            .event_queue
            .iter()
            .map(|event| {
                Ok(SimEvent {
                    event_ref: remap(event.event_ref).ok_or(event.event_ref)?,
                    ..event.clone()
                })
            })
            .collect::<Result<_, B::Event>>()?;
        self.scheduler.time = snapshot.time;
        self.scheduler.clocks.clone_from(&snapshot.clocks);
        self.scheduler.event_queue = event_queue;
        self.last_clock_values
            .clone_from(&snapshot.last_clock_values);
        self.last_clock_unknown
            .clone_from(&snapshot.last_clock_unknown);
        self.process_wakeups.clone_from(&snapshot.process_wakeups);
        self.process_state.clone_from(&snapshot.process_state);
        self.declarations_pending = snapshot.declarations_pending;
        self.host_pending = snapshot.host_pending;
        self.finished = snapshot.finished;
        self.stopped = false;
        self.suspended = None;
        Ok(())
    }
}

/// Scheduling state expressed with the event handles of one backend, so a
/// caller can translate it to and from names.
pub struct ScheduleParts<B: SimBackend> {
    pub time: u64,
    /// Periodic clocks and their periods.
    pub clocks: Vec<(B::Event, u64)>,
    /// Pending events, including those of periodic clocks.
    pub events: Vec<SimEvent<B>>,
    /// Pending events that a periodic clock re-schedules when they fire, with
    /// the number of identical such events.
    pub periodic: Vec<(SimEvent<B>, u64)>,
    /// Events whose signal was high when edge detection last sampled it.
    /// An x counts as high.
    pub high_events: Vec<B::Event>,
    /// Events whose signal was x or z when edge detection last sampled it.
    pub unknown_events: Vec<B::Event>,
    /// Suspended processes and the time each resumes at.
    pub process_wakeups: Vec<(usize, u64)>,
    /// Waiting and ready processes, queued nonblocking assignments and the
    /// counter values counted domains last triggered on.
    pub process_state: ProcessState,
    /// The host changed state the next step settles at the current time.
    pub host_pending: bool,
    /// A process requested the end of the simulation.
    pub finished: bool,
}

impl<B: SimBackend> SimulationState<B> {
    /// Express the scheduling state with `backend`'s event handles.
    pub fn export_schedule(&self, backend: &B) -> ScheduleParts<B> {
        let events = backend.id_to_event_slice();
        ScheduleParts {
            time: self.scheduler.time,
            clocks: self
                .scheduler
                .clocks
                .iter()
                .enumerate()
                .filter_map(|(id, clock)| Some((events[id], clock.as_ref()?.period)))
                .collect(),
            events: self.scheduler.event_queue.iter().cloned().collect(),
            periodic: {
                // Identical periodic events once, with their number, in the
                // order of `events`.
                let mut periodic: Vec<(SimEvent<B>, u64)> = Vec::new();
                for event in self.scheduler.event_queue.iter().filter(|e| e.periodic) {
                    match periodic.iter_mut().find(|(known, _)| known == event) {
                        Some((_, count)) => *count += 1,
                        None => periodic.push((event.clone(), 1)),
                    }
                }
                periodic
            },
            high_events: self.last_clock_values.iter().map(|id| events[id]).collect(),
            unknown_events: self
                .last_clock_unknown
                .iter()
                .map(|id| events[id])
                .collect(),
            process_wakeups: {
                let mut wakeups = self
                    .process_wakeups
                    .iter()
                    .map(|&Reverse((time, process))| (process, time))
                    .collect::<Vec<_>>();
                wakeups.sort_unstable();
                wakeups
            },
            process_state: self.process_state.clone(),
            host_pending: self.host_pending,
            finished: self.finished,
        }
    }

    /// Replace the scheduling state with `parts`, whose handles belong to
    /// this state's backend.
    pub fn import_schedule(&mut self, parts: ScheduleParts<B>) {
        self.scheduler.time = parts.time;
        self.scheduler.clocks.clear();
        for (event, period) in parts.clocks {
            let id = event.id();
            if id >= self.scheduler.clocks.len() {
                self.scheduler.clocks.resize(id + 1, None);
            }
            self.scheduler.clocks[id] = Some(ClockDef { period });
        }
        // Mark `count` pending events identical to each periodic one, the
        // first in the order of `events`.
        let mut events = parts.events;
        for event in &mut events {
            event.periodic = false;
        }
        for (periodic, count) in parts.periodic {
            for event in events
                .iter_mut()
                .filter(|event| !event.periodic && **event == periodic)
                .take(count as usize)
            {
                event.periodic = true;
            }
        }
        self.scheduler.event_queue = events.into_iter().collect();
        self.last_clock_values.make_empty();
        for event in parts.high_events {
            self.last_clock_values.insert(event.id());
        }
        self.last_clock_unknown.make_empty();
        if self.four_state {
            for event in parts.unknown_events {
                self.last_clock_unknown.insert(event.id());
            }
        }
        self.process_wakeups = parts
            .process_wakeups
            .into_iter()
            .map(|(process, time)| Reverse((time, process)))
            .collect();
        self.process_state = parts.process_state;
        self.declarations_pending = self
            .process_wakeups
            .iter()
            .any(|Reverse((_, index))| self.model.processes[*index].is_initializer)
            || self
                .process_state
                .ready
                .iter()
                .any(|&index| self.model.processes[index].is_initializer);
        self.host_pending = parts.host_pending;
        self.finished = parts.finished;
        self.stopped = false;
        self.suspended = None;
    }
}

/// Scheduling state captured by [`SimulationState::snapshot`].
pub struct SimulationSnapshot<B: SimBackend> {
    time: u64,
    clocks: Vec<Option<ClockDef>>,
    event_queue: std::collections::BinaryHeap<SimEvent<B>>,
    last_clock_values: BitSet,
    last_clock_unknown: BitSet,
    process_wakeups: BinaryHeap<ProcessWakeup>,
    process_state: ProcessState,
    declarations_pending: bool,
    host_pending: bool,
    finished: bool,
}

impl<B: SimBackend> Clone for SimulationSnapshot<B> {
    fn clone(&self) -> Self {
        Self {
            time: self.time,
            clocks: self.clocks.clone(),
            event_queue: self.event_queue.clone(),
            last_clock_values: self.last_clock_values.clone(),
            last_clock_unknown: self.last_clock_unknown.clone(),
            process_wakeups: self.process_wakeups.clone(),
            process_state: self.process_state.clone(),
            declarations_pending: self.declarations_pending,
            host_pending: self.host_pending,
            finished: self.finished,
        }
    }
}

impl<B: SimBackend> SimulationSnapshot<B> {
    /// Simulation time at which the snapshot was taken.
    pub fn time(&self) -> u64 {
        self.time
    }
}
