//! Executable roots of a state update. A clock and its reset alias are
//! events. Sampling and publication can be entered separately when other
//! events must sample the same old state.
use super::{ExecutionUnit, HashMap};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateEntry {
    /// Update after observations have been refreshed.
    Transition,
    /// Update directly from the current inputs.
    FromInputs,
    /// Evaluate all right hand sides before publication.
    Sample,
    /// Publish previously sampled writes.
    Publish,
}

#[derive(Clone, Debug)]
pub struct EventEntries<T> {
    entries: [Option<T>; 4],
}

impl UpdateEntry {
    pub fn groups_mut<E, A>(
        self,
        events: &mut HashMap<E, EventUpdate<A>>,
    ) -> impl Iterator<Item = (&E, &mut Vec<ExecutionUnit<A>>)> {
        events
            .iter_mut()
            .filter_map(move |(event, update)| Some((event, update.entry_mut(self)?)))
    }
}
impl<T> Default for EventEntries<T> {
    fn default() -> Self {
        Self {
            entries: Default::default(),
        }
    }
}

pub type EventUpdate<A> = EventEntries<Vec<ExecutionUnit<A>>>;

impl<T> EventEntries<T> {
    pub fn entry(&self, at: UpdateEntry) -> Option<&T> {
        self.entries[at as usize].as_ref()
    }
    pub fn entry_or_default(&mut self, at: UpdateEntry) -> &mut T
    where
        T: Default,
    {
        self.entries[at as usize].get_or_insert_with(T::default)
    }
    pub fn entry_mut(&mut self, at: UpdateEntry) -> Option<&mut T> {
        self.entries[at as usize].as_mut()
    }
    pub fn take(&mut self, at: UpdateEntry) -> Option<T> {
        self.entries[at as usize].take()
    }
    pub fn set(&mut self, at: UpdateEntry, value: T) {
        self.entries[at as usize] = Some(value);
    }
    pub fn map_entries<U>(&self, mut f: impl FnMut(&T) -> U) -> EventEntries<U> {
        EventEntries {
            entries: self
                .entries
                .each_ref()
                .map(|entry| entry.as_ref().map(&mut f)),
        }
    }
}

impl<A> EventUpdate<A> {
    pub fn units(&self) -> impl Iterator<Item = &ExecutionUnit<A>> {
        self.entries.iter().flatten().flatten()
    }
    pub fn units_mut(&mut self) -> impl Iterator<Item = &mut ExecutionUnit<A>> {
        self.entries.iter_mut().flatten().flatten()
    }
    pub(crate) fn map<B>(self, state: &mut impl FnMut(A) -> B) -> EventUpdate<B>
    where
        A: Clone,
    {
        EventUpdate {
            entries: self.entries.map(|entry| {
                entry.map(|units| units.into_iter().map(|u| u.into_map_addr(state)).collect())
            }),
        }
    }
}

/// The observation root and the update roots reachable from each event.
/// An absent FromInputs entry means refresh observations and enter Transition.
#[derive(Clone, Debug)]
pub struct StateUpdates<E, A> {
    pub observe: Vec<ExecutionUnit<A>>,
    /// Storage carrying values across successive observations, if known.
    pub retained: Option<Vec<A>>,
    pub events: HashMap<E, EventUpdate<A>>,
}

impl<E, A> Default for StateUpdates<E, A> {
    fn default() -> Self {
        Self {
            observe: Vec::new(),
            retained: None,
            events: HashMap::default(),
        }
    }
}
impl<E: std::hash::Hash + Eq, A> StateUpdates<E, A> {
    pub fn at(&self, event: &E, at: UpdateEntry) -> Option<&Vec<ExecutionUnit<A>>> {
        self.events.get(event)?.entry(at)
    }
    pub fn from_groups(
        observe: Vec<ExecutionUnit<A>>,
        retained: Option<Vec<A>>,
        groups: impl IntoIterator<Item = (UpdateEntry, HashMap<E, Vec<ExecutionUnit<A>>>)>,
    ) -> Self {
        let mut events = HashMap::<E, EventUpdate<A>>::default();
        for (entry, groups) in groups {
            for (event, units) in groups {
                events.entry(event).or_default().set(entry, units);
            }
        }
        Self {
            observe,
            retained,
            events,
        }
    }
}

impl<E, A> StateUpdates<E, A> {
    pub fn units(&self) -> impl Iterator<Item = &ExecutionUnit<A>> {
        self.observe
            .iter()
            .chain(self.events.values().flat_map(EventUpdate::units))
    }
    pub fn units_mut(&mut self) -> impl Iterator<Item = &mut ExecutionUnit<A>> {
        self.observe
            .iter_mut()
            .chain(self.events.values_mut().flat_map(EventUpdate::units_mut))
    }
    pub fn groups(
        &self,
        at: UpdateEntry,
    ) -> impl Iterator<Item = (&E, &Vec<ExecutionUnit<A>>)> + Clone {
        self.events
            .iter()
            .filter_map(move |(e, u)| Some((e, u.entry(at)?)))
    }
    pub fn groups_mut(
        &mut self,
        at: UpdateEntry,
    ) -> impl Iterator<Item = (&E, &mut Vec<ExecutionUnit<A>>)> {
        at.groups_mut(&mut self.events)
    }
}
