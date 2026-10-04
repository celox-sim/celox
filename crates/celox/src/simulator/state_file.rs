//! Saving and loading layout-independent state files.
//!
//! Unlike a [`super::Checkpoint`], a [`StateFile`] identifies every state
//! object by its path, so it can move between simulators built with different
//! backends, optimization levels or memory layouts. Only objects that hold
//! state across evaluations must match; combinational objects are recomputed
//! after loading.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::HashMap;

use celox_runtime::scheduler::SimEvent;
use celox_runtime::{
    ScheduleParts, ScheduleRecord, ScheduledEvent, StateFile, StateObject, StateRole,
};
use num_bigint::BigUint;

use super::checkpoint::CheckpointError;
use super::host::Simulator;
use crate::RuntimeErrorCode;
use crate::backend::{EventHandle, SimBackend};
use crate::ir::SignalRef;

/// Why a state file could not be saved or loaded.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Checkpoint(#[from] CheckpointError),
    #[error("the state file does not match the design: {0}")]
    Mismatch(StateMismatch),
    #[error("the state file has no simulation schedule; it was saved from a Simulator")]
    MissingSchedule,
    #[error("evaluating combinational logic failed: {0}")]
    Runtime(RuntimeErrorCode),
}

/// Differences between a state file and the design it is loaded into.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateMismatch {
    /// State objects of the design that the file does not contain.
    pub missing_in_file: Vec<String>,
    /// State objects of the file that the design does not declare.
    pub missing_in_design: Vec<String>,
    /// Objects declared with another width, as (path, file width, design width).
    pub width_mismatches: Vec<(String, usize, usize)>,
    /// Event or signal names of the schedule that the design does not declare.
    pub unknown_names: Vec<String>,
}

impl StateMismatch {
    fn is_empty(&self) -> bool {
        self.missing_in_file.is_empty()
            && self.missing_in_design.is_empty()
            && self.width_mismatches.is_empty()
            && self.unknown_names.is_empty()
    }
}

impl std::fmt::Display for StateMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn list(
            f: &mut std::fmt::Formatter<'_>,
            label: &str,
            items: &[String],
        ) -> std::fmt::Result {
            const SHOWN: usize = 10;
            if items.is_empty() {
                return Ok(());
            }
            write!(
                f,
                "\n  {label}: {}",
                items[..items.len().min(SHOWN)].join(", ")
            )?;
            if items.len() > SHOWN {
                write!(f, " and {} more", items.len() - SHOWN)?;
            }
            Ok(())
        }
        list(f, "missing in file", &self.missing_in_file)?;
        list(f, "missing in design", &self.missing_in_design)?;
        let widths: Vec<_> = self
            .width_mismatches
            .iter()
            .map(|(path, file, design)| format!("{path} (file {file}, design {design})"))
            .collect();
        list(f, "width differs", &widths)?;
        list(f, "unknown in schedule", &self.unknown_names)
    }
}

/// A state object of the design, by the path a state file uses for it.
#[derive(Debug, Clone)]
struct NamedObject {
    path: String,
    signal: SignalRef,
    role: StateRole,
}

/// The state objects of a built design, with the paths and roles that state
/// files use for them.
///
/// [`Simulator::save_state`] and [`Simulator::load_state`] use it internally.
/// Hosts that keep only the backend of a simulator take its schema with
/// [`Simulator::state_schema`] first.
#[derive(Debug, Clone)]
pub struct StateSchema {
    four_state: bool,
    objects: Vec<NamedObject>,
}

impl StateSchema {
    /// Save the value of every state object of `backend` as last evaluated.
    pub fn capture<B: SimBackend>(&self, backend: &B) -> StateFile {
        let objects = self
            .objects
            .iter()
            .map(|object| {
                let width = object.signal.width;
                let size = width.div_ceil(8);
                let bytes = |value: BigUint| {
                    let mut bytes = value.to_bytes_le();
                    bytes.resize(size, 0);
                    bytes
                };
                let (value, mask) = if self.four_state && object.signal.is_4state {
                    let (value, mask) = backend.get_four_state(object.signal);
                    (bytes(value), Some(bytes(mask)))
                } else {
                    (bytes(backend.get(object.signal)), None)
                };
                StateObject {
                    path: object.path.clone(),
                    width,
                    role: object.role,
                    is_4state: object.signal.is_4state,
                    value,
                    mask,
                }
            })
            .collect();
        StateFile {
            four_state: self.four_state,
            objects,
            schedule: None,
        }
    }

    /// Write the state saved in `file` into `backend`, matching objects by
    /// path. Every object that holds state in this design must be present
    /// with the same width; otherwise nothing is written and the differences
    /// are returned. The caller must re-evaluate combinational logic.
    pub fn load<B: SimBackend>(
        &self,
        backend: &mut B,
        file: &StateFile,
    ) -> Result<(), StateMismatch> {
        let saved: BTreeMap<&str, &StateObject> = file
            .objects
            .iter()
            .map(|object| (object.path.as_str(), object))
            .collect();
        let mut mismatch = StateMismatch::default();
        let mut writes = Vec::new();
        for object in &self.objects {
            if object.role == StateRole::Comb {
                continue;
            }
            match saved.get(object.path.as_str()) {
                None => mismatch.missing_in_file.push(object.path.clone()),
                Some(saved) if saved.width != object.signal.width => {
                    mismatch.width_mismatches.push((
                        object.path.clone(),
                        saved.width,
                        object.signal.width,
                    ));
                }
                Some(saved) => writes.push((object.signal, *saved)),
            }
        }
        let declared: std::collections::BTreeSet<&str> = self
            .objects
            .iter()
            .map(|object| object.path.as_str())
            .collect();
        for object in &file.objects {
            if object.role == StateRole::State && !declared.contains(object.path.as_str()) {
                mismatch.missing_in_design.push(object.path.clone());
            }
        }
        if !mismatch.is_empty() {
            return Err(mismatch);
        }
        for (signal, object) in writes {
            let value = BigUint::from_bytes_le(&object.value);
            let mask = object
                .mask
                .as_deref()
                .map(BigUint::from_bytes_le)
                .unwrap_or_default();
            if self.four_state && signal.is_4state {
                backend.set_four_state(signal, value, mask);
            } else {
                // A two-state simulation reads unknown bits as zero.
                let unknown = &value & &mask;
                backend.set_wide(signal, value - unknown);
            }
        }
        Ok(())
    }
}

impl<B: SimBackend> Simulator<B> {
    /// The state objects of this design as state files name them.
    pub fn state_schema(&self) -> Arc<StateSchema> {
        self.state_schema
            .get_or_init(|| Arc::new(self.build_state_schema()))
            .clone()
    }

    /// Save the value of every state object, by path.
    ///
    /// Combinational values are settled first, so the file shows the values
    /// a read would return.
    pub fn save_state(&mut self) -> Result<StateFile, StateError> {
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents.into());
        }
        if self.dirty {
            self.eval_comb_checked().map_err(StateError::Runtime)?;
            self.dirty = false;
        }
        Ok(self.state_schema().capture(&self.backend))
    }

    /// Load the state saved in `file`, matching objects by path.
    ///
    /// Every object that holds state in this design must be present with the
    /// same width; otherwise nothing is changed and the differences are
    /// returned. Combinational objects are recomputed. Runtime events do not
    /// fire for the jump to the loaded state. A schedule in the file is
    /// ignored. An attached VCD writer records the loaded values as changes
    /// at the next dump.
    pub fn load_state(&mut self, file: &StateFile) -> Result<(), StateError> {
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents.into());
        }
        self.state_schema()
            .load(&mut self.backend, file)
            .map_err(StateError::Mismatch)?;
        // The loaded state replaces the simulation history, so combinational
        // observers take it as their baseline instead of reporting changes.
        self.backend
            .eval_comb()
            .map_err(|error| StateError::Runtime(self.decorate_runtime_error(error)))?;
        self.comb_observer_snapshots = self.snapshot_all_comb_observers();
        self.comb_observer_initial_eval = false;
        if let Some(writer) = &mut self.vcd_writer {
            writer.rescan();
        }
        self.dirty = false;
        Ok(())
    }

    /// Name the state objects: the hierarchical path, followed by `#n` when
    /// several objects share it.
    fn build_state_schema(&self) -> StateSchema {
        let comb_writes = &self.program.runtime_schema.comb_writes;
        let mut objects: Vec<_> = self
            .backend
            .layout()
            .offsets
            .keys()
            .map(|&address| (self.program.get_path(&address), address))
            .collect();
        objects.sort_unstable();
        let mut named = Vec::with_capacity(objects.len());
        let mut index = 0;
        while index < objects.len() {
            let path = &objects[index].0;
            let end = objects[index..]
                .iter()
                .position(|(other, _)| other != path)
                .map_or(objects.len(), |offset| index + offset);
            for (ordinal, (path, address)) in objects[index..end].iter().enumerate() {
                named.push(NamedObject {
                    path: if end - index == 1 {
                        path.clone()
                    } else {
                        format!("{path}#{ordinal}")
                    },
                    signal: self.backend.resolve_signal(address),
                    role: if comb_writes.contains(address) {
                        StateRole::Comb
                    } else {
                        StateRole::State
                    },
                });
            }
            index = end;
        }
        StateSchema {
            four_state: self.backend.layout().four_state,
            objects: named,
        }
    }

    /// Express a schedule by event and signal names.
    pub(crate) fn name_schedule(&self, parts: ScheduleParts<B>) -> ScheduleRecord {
        let signal_paths: HashMap<usize, String> = self
            .state_schema()
            .objects
            .iter()
            .rev()
            .map(|object| (object.signal.offset, object.path.clone()))
            .collect();
        let event_path = |event: B::Event| self.program.get_path(&event.addr());
        // An event normally drives its own signal. Name that signal by the
        // event's path: other objects may share its storage under paths that
        // another build keeps separate.
        let signal_path = |event: &SimEvent<B>| {
            let own = self.backend.resolve_signal(&event.event_ref.addr());
            if own.offset == event.signal.offset {
                event_path(event.event_ref)
            } else {
                signal_paths
                    .get(&event.signal.offset)
                    .cloned()
                    .unwrap_or_default()
            }
        };
        let named = |event: &SimEvent<B>| ScheduledEvent {
            time: event.time,
            event: event_path(event.event_ref),
            signal: signal_path(event),
            value: event.next_val,
        };
        ScheduleRecord {
            time: parts.time,
            clocks: parts
                .clocks
                .into_iter()
                .map(|(event, period)| (event_path(event), period))
                .collect(),
            events: parts.events.iter().map(named).collect(),
            periodic: parts
                .periodic
                .iter()
                .map(|(event, count)| (named(event), *count))
                .collect(),
            high_events: parts.high_events.into_iter().map(event_path).collect(),
        }
    }

    /// Resolve a named schedule into this design's handles. Fails with the
    /// names the design does not declare.
    pub(crate) fn resolve_schedule(
        &self,
        record: &ScheduleRecord,
    ) -> Result<ScheduleParts<B>, StateError> {
        let events: HashMap<String, B::Event> = self
            .backend
            .id_to_event_slice()
            .iter()
            .map(|&event| (self.program.get_path(&event.addr()), event))
            .collect();
        let signals: HashMap<String, SignalRef> = self
            .state_schema()
            .objects
            .iter()
            .map(|object| (object.path.clone(), object.signal))
            .collect();
        let scheduled = record
            .events
            .iter()
            .chain(record.periodic.iter().map(|(event, _)| event));
        let mut unknown: Vec<String> = record
            .clocks
            .iter()
            .map(|(name, _)| name)
            .chain(&record.high_events)
            .chain(scheduled.clone().map(|event| &event.event))
            .filter(|name| !events.contains_key(*name))
            .chain(
                scheduled
                    .map(|event| &event.signal)
                    .filter(|name| !signals.contains_key(*name)),
            )
            .cloned()
            .collect();
        if !unknown.is_empty() {
            unknown.sort();
            unknown.dedup();
            return Err(StateError::Mismatch(StateMismatch {
                unknown_names: unknown,
                ..Default::default()
            }));
        }
        let resolve = |event: &ScheduledEvent| SimEvent {
            time: event.time,
            event_ref: events[&event.event],
            signal: signals[&event.signal],
            next_val: event.value,
        };
        Ok(ScheduleParts {
            time: record.time,
            clocks: record
                .clocks
                .iter()
                .map(|(name, period)| (events[name], *period))
                .collect(),
            events: record.events.iter().map(resolve).collect(),
            periodic: record
                .periodic
                .iter()
                .map(|(event, count)| (resolve(event), *count))
                .collect(),
            high_events: record.high_events.iter().map(|name| events[name]).collect(),
        })
    }
}
