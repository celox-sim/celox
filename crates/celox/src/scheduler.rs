use crate::{EventRef, ir::SignalRef};
use std::collections::BinaryHeap;

#[derive(Debug, Clone)]
pub struct ClockDef {
    pub period: u64,
}

#[derive(Debug, Clone)]
pub struct SimEvent {
    pub time: u64,
    pub event_ref: EventRef,
    pub signal: SignalRef,
    pub next_val: u8,
}

impl PartialEq for SimEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
            && self.event_ref.addr == other.event_ref.addr
            && self.signal == other.signal
            && self.next_val == other.next_val
    }
}

impl Eq for SimEvent {}

impl PartialOrd for SimEvent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SimEvent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Earlier time has higher priority (BinaryHeap is a Max-Heap)
        other
            .time
            .cmp(&self.time)
            .then_with(|| {
                let id1 = self.event_ref.id;
                let id2 = other.event_ref.id;
                id2.cmp(&id1)
            })
            .then_with(|| other.signal.cmp(&self.signal))
    }
}

pub struct Scheduler {
    pub(crate) time: u64,
    pub(crate) clocks: Vec<Option<ClockDef>>,
    pub(crate) event_queue: BinaryHeap<SimEvent>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            time: 0,
            clocks: Vec::new(),
            event_queue: BinaryHeap::new(),
        }
    }

    pub fn next_event_time(&self) -> Option<u64> {
        self.event_queue.peek().map(|e| e.time)
    }

    pub fn push(&mut self, event: SimEvent) {
        self.event_queue.push(event);
    }

    pub fn pop_all_at_next_time(&mut self) -> Option<(u64, Vec<SimEvent>)> {
        let next_time = self.next_event_time()?;
        let mut events = Vec::new();
        while let Some(ev) = self.event_queue.peek() {
            if ev.time == next_time {
                events.push(self.event_queue.pop().unwrap());
            } else {
                break;
            }
        }
        Some((next_time, events))
    }
}
