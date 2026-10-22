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
use celox_runtime::timing::Baseline;
use celox_runtime::{
    BaselineRecord, NbaRecord, ProcessRecord, ProcessState, ScheduleParts, ScheduleRecord,
    ScheduledEvent, StateFile, StateObject, StateRole, WaiterRecord,
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
    /// Not returned: state files record suspended processes. Kept so that
    /// matching code keeps compiling.
    #[error("the state of the simulation's processes could not be recorded")]
    Processes,
    /// Not returned: state files record strings by their contents. Kept so
    /// that matching code keeps compiling.
    #[error("state files do not support dynamic testbench strings; use a checkpoint instead")]
    TestbenchStrings,
    /// A variable holds a class object, which state files cannot record.
    #[error("state files do not record class objects; `{0}` holds one, use a checkpoint instead")]
    Objects(String),
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
    /// Whether the elements are string handles, which a file records as
    /// references to its strings section.
    strings: bool,
    /// Whether the elements are object handles, which a file cannot record.
    objects: bool,
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
    ///
    /// Fails if a variable holds a class object.
    pub fn capture<B: SimBackend>(&self, backend: &B) -> Result<StateFile, StateError> {
        let heap = backend.heap();
        let mut strings = Vec::new();
        let mut file_ids: HashMap<celox_heap::Handle, u32> = HashMap::default();
        let mut objects: Vec<StateObject> = self
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
        // Number strings by first use in path order, so equal states save
        // equal files.
        for (object, named) in objects.iter_mut().zip(&self.objects) {
            if named.objects && object.value.iter().any(|&byte| byte != 0) {
                return Err(StateError::Objects(named.path.clone()));
            }
            if !named.strings {
                continue;
            }
            for element in object
                .value
                .as_chunks_mut::<{ celox_heap::HANDLE_BYTES }>()
                .0
            {
                let handle = celox_heap::Handle::from_le_bytes(*element);
                let contents = heap.string(handle);
                let id = if contents.is_empty() {
                    0
                } else {
                    // Interning gives each contents one handle.
                    *file_ids.entry(handle).or_insert_with(|| {
                        strings.push(contents.to_vec());
                        strings.len() as u32
                    })
                };
                element.copy_from_slice(&id.to_le_bytes());
            }
        }
        Ok(StateFile {
            four_state: self.four_state,
            objects,
            schedule: None,
            strings,
        })
    }

    /// Write the state saved in `file` into `backend`, matching objects by
    /// path. Every object that holds state in this design must be present
    /// with the same width; otherwise nothing is written and the differences
    /// are returned. Saved combinational values are written too, as starting
    /// points for combinational loops. The caller must re-evaluate
    /// combinational logic.
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
        // Combinational values are recomputed, but a combinational loop (a
        // latch made of gates) settles from its current value, so it starts
        // from the saved one. They are written before the state, which wins
        // where a combinational alias shares a register's storage.
        let mut seeds = Vec::new();
        let mut writes = Vec::new();
        for object in &self.objects {
            if object.role == StateRole::Comb {
                if let Some(saved) = saved.get(object.path.as_str())
                    && saved.width == object.signal.width
                {
                    seeds.push((object, *saved));
                }
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
                Some(saved) => writes.push((object, *saved)),
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
        for (named, object) in seeds.into_iter().chain(writes) {
            let signal = named.signal;
            let value = if named.strings {
                let mut value = object.value.clone();
                for element in value.as_chunks_mut::<{ celox_heap::HANDLE_BYTES }>().0 {
                    let id = u32::from_le_bytes(*element) as usize;
                    let contents = id
                        .checked_sub(1)
                        .and_then(|index| file.strings.get(index))
                        .map_or(&[][..], Vec::as_slice);
                    element.copy_from_slice(&backend.heap_mut().intern(contents).to_le_bytes());
                }
                BigUint::from_bytes_le(&value)
            } else {
                BigUint::from_bytes_le(&object.value)
            };
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
        self.check_host_checkpoint_support()?;
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents.into());
        }
        if self.dirty {
            self.eval_comb_checked().map_err(StateError::Runtime)?;
            self.dirty = false;
        }
        self.state_schema().capture(&self.backend)
    }

    /// Load the state saved in `file`, matching objects by path.
    ///
    /// Every object that holds state in this design must be present with the
    /// same width; otherwise nothing is changed and the differences are
    /// returned. If evaluating the loaded state fails, the previous state is
    /// put back and the error returned. Combinational objects are recomputed. Runtime events do not
    /// fire for the jump to the loaded state. A schedule in the file is
    /// ignored. An attached VCD writer records the loaded values as changes
    /// at the next dump.
    pub fn load_state(&mut self, file: &StateFile) -> Result<(), StateError> {
        self.load_state_at_time(file, 0)
    }

    /// Publish time after validating the objects and before settling them.
    /// The checkpoint also rolls this publication back if evaluation fails.
    pub(crate) fn load_state_at_time(
        &mut self,
        file: &StateFile,
        time: u64,
    ) -> Result<(), StateError> {
        if !self.components.is_empty() {
            return Err(CheckpointError::ExternalComponents.into());
        }
        // Evaluating the loaded state can still fail (an oscillating
        // combinational loop), so keep the current state to return to.
        let previous = self.checkpoint()?;
        self.state_schema()
            .load(&mut self.backend, file)
            .map_err(StateError::Mismatch)?;
        for address in &self.program.runtime_schema.time_slots {
            self.backend.set(
                SignalRef {
                    offset: self.backend.layout().offsets[address],
                    width: celox_design::TIME_SLOT_WIDTH,
                    is_4state: false,
                    array_layout: None,
                },
                time,
            );
        }
        // The loaded state replaces the simulation history, so combinational
        // observers take it as their baseline instead of reporting changes.
        if let Err(error) = self.backend.eval_comb() {
            let error = self.decorate_runtime_error(error);
            self.restore(&previous)?;
            return Err(StateError::Runtime(error));
        }
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
        // A timed simulation stores its time into the time slots, and a state
        // file keeps the time in its schedule; an untimed one reads zero.
        // A compiled scheduler's slots hold nothing between runs.
        let time_slots: crate::HashSet<_> = self
            .program
            .runtime_schema
            .time_slots
            .iter()
            .chain(
                self.program
                    .runtime_schema
                    .scheduler
                    .iter()
                    .flat_map(|scheduler| scheduler.slots.iter()),
            )
            .collect();
        let mut objects: Vec<_> = self
            .backend
            .layout()
            .offsets
            .keys()
            .filter(|address| !time_slots.contains(address))
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
                    strings: self.program.design.state_objects.get(address).is_some_and(
                        |metadata| metadata.value_kind == celox_design::ValueKind::StrRef,
                    ),
                    objects: self.program.design.state_objects.get(address).is_some_and(
                        |metadata| metadata.value_kind == celox_design::ValueKind::ObjRef,
                    ),
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
        let processes = self.name_processes(&parts, &signal_paths);
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
            unknown_events: parts.unknown_events.into_iter().map(event_path).collect(),
            processes,
        }
    }

    /// The status slot of each process, by which a state file names it.
    fn process_statuses(&self) -> Vec<SignalRef> {
        self.program
            .runtime_schema
            .processes
            .iter()
            .map(|slots| self.backend.resolve_signal(&slots.status))
            .collect()
    }

    /// The signal of each counted signal, in counter order.
    fn counted_signals(&self) -> Vec<SignalRef> {
        self.program
            .runtime_schema
            .watches
            .iter()
            .map(|watch| self.backend.resolve_signal(&watch.signal))
            .collect()
    }

    fn name_processes(
        &self,
        parts: &ScheduleParts<B>,
        signal_paths: &HashMap<usize, String>,
    ) -> ProcessRecord {
        let path = |signal: SignalRef| {
            signal_paths
                .get(&signal.offset)
                .cloned()
                .unwrap_or_default()
        };
        let statuses = self.process_statuses();
        let process = |index: usize| path(statuses[index]);
        let state = &parts.process_state;
        ProcessRecord {
            wakeups: parts
                .process_wakeups
                .iter()
                .map(|&(index, time)| (process(index), time))
                .collect(),
            waiters: state
                .waiters
                .iter()
                .map(|(index, waiter)| {
                    // The widths of the site's term signals split a plane
                    // image into its value and mask.
                    let widths: Vec<usize> = self
                        .program
                        .runtime_schema
                        .wait_sites
                        .iter()
                        .filter(|site| site.process == index)
                        .nth(waiter.site)
                        .map(|site| {
                            site.terms
                                .iter()
                                .map(|(signal, _)| self.backend.resolve_signal(signal).width)
                                .collect()
                        })
                        .unwrap_or_default();
                    WaiterRecord {
                        process: process(index),
                        site: waiter.site as u64,
                        baselines: waiter
                            .baselines
                            .iter()
                            .enumerate()
                            .map(|(term, baseline)| match baseline {
                                Baseline::Counter {
                                    seen,
                                    value: (value, mask),
                                } => BaselineRecord::Counter {
                                    seen: *seen,
                                    value: value.to_bytes_le(),
                                    mask: mask.to_bytes_le(),
                                },
                                Baseline::Value((value, mask)) => BaselineRecord::Value {
                                    value: value.to_bytes_le(),
                                    mask: mask.to_bytes_le(),
                                },
                                Baseline::Raw(image) => {
                                    let bytes = widths
                                        .get(term)
                                        .map_or(image.len(), |width| width.div_ceil(8).max(1))
                                        .min(image.len());
                                    BaselineRecord::Value {
                                        value: image[..bytes].to_vec(),
                                        mask: image[bytes..].to_vec(),
                                    }
                                }
                            })
                            .collect(),
                    }
                })
                .collect(),
            ready: state.ready.iter().map(|&index| process(index)).collect(),
            inactive: state.inactive.iter().map(|&index| process(index)).collect(),
            nba_queue: state
                .nba_queue
                .iter()
                .map(|update| NbaRecord {
                    target: path(update.target),
                    lsb: update.lsb as u64,
                    width: update.width as u64,
                    value: update.value.0.to_bytes_le(),
                    mask: update.value.1.to_bytes_le(),
                })
                .collect(),
            domain_counters: self
                .counted_signals()
                .into_iter()
                .zip(&state.domain_counters)
                .map(|(signal, counters)| (path(signal), *counters))
                .collect(),
            host_writes: Vec::new(),
            host_pending: parts.host_pending,
            finished: parts.finished,
        }
    }

    /// The host writes a timed simulation has yet to show its waits, by
    /// signal path.
    pub(crate) fn name_host_writes(&self) -> Vec<(String, Vec<u8>, Vec<u8>)> {
        let schema = self.state_schema();
        self.host_watch
            .log()
            .iter()
            .filter_map(|(signal, (value, mask))| {
                let object = schema
                    .objects
                    .iter()
                    .find(|object| object.signal.offset == signal.offset)?;
                Some((object.path.clone(), value.to_bytes_le(), mask.to_bytes_le()))
            })
            .collect()
    }

    /// Resolve logged host writes, or the signal names this design does not
    /// declare.
    pub(crate) fn resolve_host_writes(
        &self,
        writes: &[(String, Vec<u8>, Vec<u8>)],
    ) -> Result<Vec<(SignalRef, celox_runtime::timing::Value)>, StateError> {
        let schema = self.state_schema();
        let mut unknown = Vec::new();
        let resolved = writes
            .iter()
            .filter_map(|(path, value, mask)| {
                let object = schema.objects.iter().find(|object| &object.path == path);
                if object.is_none() {
                    unknown.push(path.clone());
                }
                Some((
                    object?.signal,
                    (BigUint::from_bytes_le(value), BigUint::from_bytes_le(mask)),
                ))
            })
            .collect();
        if unknown.is_empty() {
            Ok(resolved)
        } else {
            Err(StateError::Mismatch(StateMismatch {
                unknown_names: unknown,
                ..Default::default()
            }))
        }
    }

    /// Resolve the process section of a schedule, or the names and sites
    /// this design does not declare.
    fn resolve_processes(
        &self,
        record: &ProcessRecord,
        signals: &HashMap<String, SignalRef>,
    ) -> Result<(Vec<(usize, u64)>, ProcessState), Vec<String>> {
        let statuses = self.process_statuses();
        let processes: HashMap<String, usize> = statuses
            .iter()
            .enumerate()
            .filter_map(|(index, status)| {
                let path = signals
                    .iter()
                    .find(|(_, signal)| signal.offset == status.offset)?
                    .0;
                Some((path.clone(), index))
            })
            .collect();
        let mut unknown: Vec<String> = record
            .wakeups
            .iter()
            .map(|(name, _)| name)
            .chain(&record.ready)
            .chain(&record.inactive)
            .chain(record.waiters.iter().map(|waiter| &waiter.process))
            .filter(|name| !processes.contains_key(*name))
            .cloned()
            .collect();
        let process = |name: &String| processes.get(name).copied();
        let wakeups: Vec<_> = record
            .wakeups
            .iter()
            .filter_map(|(name, time)| Some((process(name)?, *time)))
            .collect();
        let ready = record.ready.iter().filter_map(process).collect();
        let inactive = record.inactive.iter().filter_map(process).collect();
        let mut waiters = celox_runtime::timing::Waiters::default();
        for waiter in &record.waiters {
            let Some(index) = process(&waiter.process) else {
                continue;
            };
            let sites = self
                .program
                .runtime_schema
                .wait_sites
                .iter()
                .filter(|site| site.process == index)
                .collect::<Vec<_>>();
            let site = usize::try_from(waiter.site).ok();
            // Each baseline has the kind of its term: a counter for a
            // counted signal, a value otherwise.
            let counted = |signal: &crate::AbsoluteAddr| {
                self.program
                    .runtime_schema
                    .watches
                    .iter()
                    .any(|watch| watch.signal == *signal)
            };
            if !site.and_then(|site| sites.get(site)).is_some_and(|site| {
                site.terms.len() == waiter.baselines.len()
                    && site
                        .terms
                        .iter()
                        .zip(&waiter.baselines)
                        .all(|((signal, _), baseline)| {
                            counted(signal) == matches!(baseline, BaselineRecord::Counter { .. })
                        })
            }) {
                unknown.push(format!("{} wait site {}", waiter.process, waiter.site));
                continue;
            }
            let driven = self
                .program
                .runtime_schema
                .wait_sites
                .iter()
                .filter(|site| site.process == index)
                .nth(site.unwrap_or_default())
                .is_some_and(|site| {
                    site.terms.iter().any(|(signal, _)| {
                        self.program
                            .runtime_schema
                            .rtl_writes
                            .iter()
                            .any(|atom| atom.id == *signal)
                    })
                });
            waiters.insert(
                index,
                driven,
                celox_runtime::timing::Waiter {
                    site: site.unwrap_or_default(),
                    baselines: waiter
                        .baselines
                        .iter()
                        .map(|baseline| match baseline {
                            BaselineRecord::Counter { seen, value, mask } => Baseline::Counter {
                                seen: *seen,
                                value: (
                                    BigUint::from_bytes_le(value),
                                    BigUint::from_bytes_le(mask),
                                ),
                            },
                            BaselineRecord::Value { value, mask } => Baseline::Value((
                                BigUint::from_bytes_le(value),
                                BigUint::from_bytes_le(mask),
                            )),
                        })
                        .collect(),
                },
            );
        }
        let mut nba_queue = Vec::new();
        for update in &record.nba_queue {
            match signals.get(&update.target) {
                // A queued assignment covers a nonempty range of its target.
                Some(&target)
                    if update.width == 0
                        || update
                            .lsb
                            .checked_add(update.width)
                            .is_none_or(|end| end > target.width as u64) =>
                {
                    unknown.push(format!(
                        "{} [{} +: {}]",
                        update.target, update.lsb, update.width
                    ));
                }
                Some(&target) => nba_queue.push(celox_runtime::NbaUpdate {
                    target,
                    lsb: update.lsb as usize,
                    width: update.width as usize,
                    value: (
                        BigUint::from_bytes_le(&update.value),
                        BigUint::from_bytes_le(&update.mask),
                    ),
                }),
                None => unknown.push(update.target.clone()),
            }
        }
        let counted = self.counted_signals();
        let mut domain_counters = vec![[0; 3]; counted.len()];
        for (name, counters) in &record.domain_counters {
            let index = signals
                .get(name)
                .and_then(|signal| counted.iter().position(|counted| counted == signal));
            match index {
                Some(index) => domain_counters[index] = *counters,
                None => unknown.push(name.clone()),
            }
        }
        if !unknown.is_empty() {
            return Err(unknown);
        }
        Ok((
            wakeups,
            ProcessState {
                waiters,
                ready,
                inactive,
                nba_queue,
                domain_counters,
            },
        ))
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
            .chain(&record.unknown_events)
            .chain(scheduled.clone().map(|event| &event.event))
            .filter(|name| !events.contains_key(*name))
            .chain(
                scheduled
                    .map(|event| &event.signal)
                    .filter(|name| !signals.contains_key(*name)),
            )
            .cloned()
            .collect();
        let processes = self.resolve_processes(&record.processes, &signals);
        if let Err(names) = &processes {
            unknown.extend(names.iter().cloned());
        }
        if !unknown.is_empty() {
            unknown.sort();
            unknown.dedup();
            return Err(StateError::Mismatch(StateMismatch {
                unknown_names: unknown,
                ..Default::default()
            }));
        }
        let (process_wakeups, process_state) = processes.expect("checked above");
        let resolve = |event: &ScheduledEvent| SimEvent {
            time: event.time,
            event_ref: events[&event.event],
            signal: signals[&event.signal],
            next_val: event.value,
            // `import_schedule` marks the periodic events.
            periodic: false,
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
            unknown_events: record
                .unknown_events
                .iter()
                .map(|name| events[name])
                .collect(),
            process_wakeups,
            process_state,
            host_pending: record.processes.host_pending,
            finished: record.processes.finished,
        })
    }
}
