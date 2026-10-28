use std::{
    collections::BTreeMap,
    ffi::{CString, c_char, c_void},
    ptr,
};

use fxhash::FxHashMap as HashMap;

use super::{
    HandleKind, ObjectRef, VpiHandle, VpiValue, borrow_cell, borrow_cell_mut, handle_mut,
    host_is_active, new_callback_handle, object_ref, vpi_get_value, with_host,
    write_snapshot_value,
};

pub const CB_VALUE_CHANGE: i32 = 1;
pub const CB_READ_WRITE_SYNCH: i32 = 6;
pub const CB_READ_ONLY_SYNCH: i32 = 7;
pub const CB_NEXT_SIM_TIME: i32 = 8;
pub const CB_AFTER_DELAY: i32 = 9;
pub const CB_START_OF_SIMULATION: i32 = 11;
pub const CB_END_OF_SIMULATION: i32 = 12;

pub const VPI_SCALED_REAL_TIME: i32 = 1;
pub const VPI_SIM_TIME: i32 = 2;
pub const VPI_SUPPRESS_TIME: i32 = 3;
const VPI_STOP: i32 = 66;
use super::VPI_FINISH;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VpiTime {
    pub type_: i32,
    pub high: u32,
    pub low: u32,
    pub real: f64,
}

pub type VpiCallbackFn = unsafe extern "C" fn(*mut VpiCbData) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VpiCbData {
    pub reason: i32,
    pub cb_rtn: Option<VpiCallbackFn>,
    pub obj: VpiHandle,
    pub time: *mut VpiTime,
    pub value: *mut VpiValue,
    pub index: i32,
    pub user_data: *mut c_char,
}

#[repr(C)]
pub struct VpiVlogInfo {
    pub argc: i32,
    pub argv: *mut *mut c_char,
    pub product: *mut c_char,
    pub version: *mut c_char,
}

#[repr(C)]
pub struct VpiErrorInfo {
    pub state: i32,
    pub level: i32,
    pub message: *mut c_char,
    pub product: *mut c_char,
    pub code: *mut c_char,
    pub file: *mut c_char,
    pub line: i32,
}

pub(super) struct Registration {
    data: VpiCbData,
    signal: Option<super::SignalSelection>,
    _time_storage: Option<Box<VpiTime>>,
    _value_storage: Option<Box<VpiValue>>,
    pub(super) handle: VpiHandle,
    due: Option<u64>,
    snapshot: Option<ValueSnapshot>,
}

#[derive(Default)]
pub(super) struct CallbackState {
    next_id: u64,
    pub(super) callbacks: BTreeMap<u64, Registration>,
    firing: HashMap<u64, bool>,
    time: u64,
    pub(super) finish: bool,
    pub(super) running: bool,
    /// The time whose ReadOnly region a timed simulation ran last.
    read_only_at: Option<u64>,
    /// ReadOnly callbacks are running: VPI writes are rejected.
    pub(super) read_only: bool,
    /// Rounds a timed simulation spent at the current time: settles, region
    /// passes and due timers. Bounded, so that callbacks that never let the
    /// time step end report it instead of hanging.
    rounds_at_time: usize,
    pub(super) detach_on_stop: bool,
    error: Option<String>,
}

pub(super) struct VlogStorage {
    _args: Vec<CString>,
    arg_ptrs: Vec<*mut c_char>,
}

fn callback_signal(data: &VpiCbData) -> Option<super::SignalSelection> {
    if data.reason != CB_VALUE_CHANGE {
        return None;
    }
    // Safety: callback object handles obey the same contract as all VPI APIs.
    match unsafe { object_ref(data.obj) } {
        Some(ObjectRef::Signal(id)) => Some(id),
        _ => None,
    }
}

fn callback_delay(data: &VpiCbData) -> u64 {
    if data.time.is_null() {
        return 0;
    }
    // Safety: VPI registration requires the time pointer to remain readable
    // until registration returns; the value is copied here immediately.
    let time = unsafe { &*data.time };
    if time.type_ == VPI_SCALED_REAL_TIME {
        time.real.round() as u64
    } else {
        (u64::from(time.high) << 32) | u64::from(time.low)
    }
}

fn registration_ids(reason: i32) -> Vec<u64> {
    with_host(|host| registration_ids_for_host(host, reason))
}

fn registration_ids_for_host(host: &super::VpiHost, reason: i32) -> Vec<u64> {
    borrow_cell(&host.callbacks, |state| {
        state
            .callbacks
            .iter()
            .filter_map(|(&id, registration)| (registration.data.reason == reason).then_some(id))
            .collect()
    })
}

/// Keep narrow callback samples in machine words. The representation is
/// canonical in registrations, including samples from forces and timed/array runtimes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ValueSnapshot {
    Word(u64, u64),
    Wide(celox::BigUint, celox::BigUint),
}

impl ValueSnapshot {
    pub(super) fn new(value: celox::BigUint, mask: celox::BigUint) -> Self {
        if value.bits() <= 64 && mask.bits() <= 64 {
            Self::Word(
                value.iter_u64_digits().next().unwrap_or(0),
                mask.iter_u64_digits().next().unwrap_or(0),
            )
        } else {
            Self::Wide(value, mask)
        }
    }

    pub(super) fn canonical(self) -> Self {
        match self {
            Self::Word(..) => self,
            Self::Wide(value, mask) => Self::new(value, mask),
        }
    }

    pub(super) fn into_big(self) -> (celox::BigUint, celox::BigUint) {
        match self {
            Self::Word(value, mask) => (value.into(), mask.into()),
            Self::Wide(value, mask) => (value, mask),
        }
    }
}

fn changed_values(host: &super::VpiHost) -> Vec<(u64, ValueSnapshot)> {
    // Clear before collecting/firing: a callback's write must invalidate this
    // scan, including writes made by earlier callbacks in the same batch.
    if host.poll_only_when_dirty && !host.values_dirty.replace(false) {
        return Vec::new();
    }
    borrow_cell(&host.callbacks, |state| {
        state
            .callbacks
            .iter()
            .filter_map(|(&id, registration)| {
                let signal = registration.signal?;
                let (snapshot, _) = super::value_snapshot_at(host, signal)?;
                let snapshot = snapshot.canonical();
                registration
                    .snapshot
                    .as_ref()
                    .is_none_or(|previous| previous != &snapshot)
                    .then_some((id, snapshot))
            })
            .collect()
    })
}

pub(super) fn dispatch_value_changes() -> bool {
    with_host(dispatch_value_changes_for_host)
}

pub(super) fn dispatch_value_changes_for_host(host: &super::VpiHost) -> bool {
    if borrow_cell(&host.callbacks, |state| state.finish) {
        return false;
    }
    let changed = changed_values(host);
    let progressed = !changed.is_empty();
    for (id, snapshot) in changed {
        fire_with_snapshot_for_host(host, id, Some(snapshot));
        if borrow_cell(&host.callbacks, |state| state.finish) {
            break;
        }
    }
    progressed
}

fn due_ids() -> Vec<u64> {
    with_host(due_ids_for_host)
}

fn due_ids_for_host(host: &super::VpiHost) -> Vec<u64> {
    borrow_cell(&host.callbacks, |state| {
        state
            .callbacks
            .iter()
            .filter_map(|(&id, registration)| {
                (registration.data.reason == CB_AFTER_DELAY
                    && registration.due.is_some_and(|due| due <= state.time))
                .then_some(id)
            })
            .collect()
    })
}

fn fire(id: u64) -> bool {
    fire_with_snapshot(id, None)
}

fn fire_with_snapshot(id: u64, delivered_snapshot: Option<ValueSnapshot>) -> bool {
    with_host(|host| fire_with_snapshot_for_host(host, id, delivered_snapshot))
}

fn fire_with_snapshot_for_host(
    host: &super::VpiHost,
    id: u64,
    delivered_snapshot: Option<ValueSnapshot>,
) -> bool {
    let Some(mut registration) = borrow_cell_mut(&host.callbacks, |state| {
        let registration = state.callbacks.remove(&id)?;
        state.firing.insert(id, false);
        Some(registration)
    }) else {
        return false;
    };

    let now = borrow_cell(&host.callbacks, |state| state.time);
    let delivered_snapshot = delivered_snapshot.or_else(|| {
        registration.signal.and_then(|signal| {
            super::value_snapshot_at(host, signal).map(|(snapshot, _)| snapshot.canonical())
        })
    });
    if !registration.data.time.is_null() {
        // Safety: cocotb stores callback time in its live callback object.
        let time = unsafe { &mut *registration.data.time };
        match time.type_ {
            VPI_SUPPRESS_TIME => {}
            VPI_SCALED_REAL_TIME => {
                time.high = 0;
                time.low = 0;
                time.real = now as f64;
            }
            _ => {
                time.type_ = VPI_SIM_TIME;
                time.high = (now >> 32) as u32;
                time.low = now as u32;
                time.real = 0.0;
            }
        }
    }

    if registration.data.reason == CB_VALUE_CHANGE && !registration.data.value.is_null() {
        // Safety: callback registration keeps both pointers live until the
        // callback is removed. A dispatch snapshot preserves the value which
        // selected the whole callback batch even if an earlier callback wrote.
        if let Some(snapshot) = &delivered_snapshot {
            unsafe {
                write_snapshot_value(
                    host,
                    registration.data.obj,
                    registration.data.value,
                    snapshot,
                )
            };
        } else {
            unsafe { vpi_get_value(registration.data.obj, registration.data.value) };
        }
    }

    if let Some(callback) = registration.data.cb_rtn {
        // Safety: the function and callback storage were supplied by the VPI
        // module and remain live until it removes or fires the registration.
        unsafe { callback(&mut registration.data) };
    }

    let cancelled = borrow_cell_mut(&host.callbacks, |state| {
        state.firing.remove(&id).unwrap_or(true)
    });
    if registration.data.reason == CB_VALUE_CHANGE && !cancelled {
        registration.snapshot = delivered_snapshot;
        borrow_cell_mut(&host.callbacks, |state| {
            state.callbacks.insert(id, registration);
        });
    } else {
        // Safety: callback handles are private allocations and one-shot
        // callbacks are invalid as soon as their callback returns.
        drop(unsafe { Box::from_raw(registration.handle) });
    }
    true
}

fn fire_all(reason: i32) -> bool {
    with_host(|host| fire_all_for_host(host, reason))
}

fn fire_all_for_host(host: &super::VpiHost, reason: i32) -> bool {
    let ids = registration_ids_for_host(host, reason);
    let progressed = !ids.is_empty();
    for id in ids {
        fire_with_snapshot_for_host(host, id, None);
        if borrow_cell(&host.callbacks, |state| state.finish) {
            break;
        }
    }
    progressed
}

pub(super) fn run() -> bool {
    with_host(run_with_host)
}

fn run_with_host(host: &super::VpiHost) -> bool {
    borrow_cell_mut(&host.callbacks, |state| state.running = true);
    fire_all_for_host(host, CB_START_OF_SIMULATION);
    if !borrow_cell(&host.callbacks, |state| state.finish) {
        super::flush_pending_writes_for_host(host);
    }
    let mut iterations = 0usize;
    'scheduler: loop {
        if borrow_cell(&host.callbacks, |state| state.finish) {
            break;
        }
        iterations += 1;
        if iterations > 1_000_000 {
            fail_for_host(
                host,
                "VPI callback scheduler exceeded 1000000 iterations without advancing time"
                    .to_string(),
            );
            break;
        }

        let mut progressed = false;
        for id in due_ids_for_host(host) {
            progressed |= fire_with_snapshot_for_host(host, id, None);
            if borrow_cell(&host.callbacks, |state| state.finish) {
                break 'scheduler;
            }
        }
        if !super::flush_pending_writes_for_host(host) {
            break;
        }
        if borrow_cell(&host.callbacks, |state| state.finish) {
            break;
        }
        loop {
            if dispatch_value_changes_for_host(host) {
                progressed = true;
                if borrow_cell(&host.callbacks, |state| state.finish) {
                    break 'scheduler;
                }
                if !super::flush_pending_writes_for_host(host) {
                    break 'scheduler;
                }
                if borrow_cell(&host.callbacks, |state| state.finish) {
                    break 'scheduler;
                }
                continue;
            }
            if super::flush_inertial_writes_for_host(host) {
                iterations += 1;
                if iterations > 1_000_000 {
                    fail_for_host(
                        host,
                        "VPI inertial updates exceeded 1000000 rounds without advancing time"
                            .into(),
                    );
                    break 'scheduler;
                }
                progressed = true;
                if borrow_cell(&host.callbacks, |state| state.finish) {
                    break 'scheduler;
                }
                continue;
            }
            let read_write = registration_ids_for_host(host, CB_READ_WRITE_SYNCH);
            if !read_write.is_empty() {
                for id in read_write {
                    progressed |= fire_with_snapshot_for_host(host, id, None);
                    if borrow_cell(&host.callbacks, |state| state.finish) {
                        break 'scheduler;
                    }
                }
                if !super::flush_pending_writes_for_host(host) {
                    break 'scheduler;
                }
                if borrow_cell(&host.callbacks, |state| state.finish) {
                    break 'scheduler;
                }
                continue;
            }
            break;
        }
        progressed |= fire_read_only_for_host(host);

        if borrow_cell(&host.callbacks, |state| state.finish) {
            break;
        }
        if progressed {
            continue;
        }

        let next_time = borrow_cell(&host.callbacks, |state| {
            state
                .callbacks
                .values()
                .filter_map(|registration| registration.due)
                .min()
        });
        let Some(next_time) = next_time else {
            break;
        };
        borrow_cell_mut(&host.callbacks, |state| {
            if next_time > state.time {
                iterations = 0;
            }
            state.time = next_time;
        });
        fire_all_for_host(host, CB_NEXT_SIM_TIME);
        if !borrow_cell(&host.callbacks, |state| state.finish) {
            super::flush_pending_writes_for_host(host);
        }
    }
    let finished = borrow_cell(&host.callbacks, |state| state.finish);
    // End-of-simulation callbacks are the one region intentionally dispatched
    // after finish has already been requested.
    for id in registration_ids_for_host(host, CB_END_OF_SIMULATION) {
        fire_with_snapshot_for_host(host, id, None);
    }
    borrow_cell_mut(&host.callbacks, |state| state.running = false);
    finished
}

/// The signals of the value-change callbacks: those whose edges a timed
/// simulation shows before its domains commit.
fn watched_signals() -> Vec<celox::SignalRef> {
    let selections = with_host(|host| {
        borrow_cell(&host.callbacks, |state| {
            state
                .callbacks
                .values()
                .filter_map(|registration| registration.signal)
                .collect::<Vec<_>>()
        })
    });
    super::with_runtime(|runtime| {
        selections
            .into_iter()
            .filter_map(|selection| selection.signal(runtime))
            .collect()
    })
    .unwrap_or_default()
}

fn set_time(time: u64) {
    with_host(|host| {
        borrow_cell_mut(&host.callbacks, |state| {
            if state.time != time {
                state.rounds_at_time = 0;
            }
            state.time = time;
        })
    });
}

/// The rounds a timed simulation may spend at one time.
const ROUNDS_AT_TIME: usize = 1_000_000;

/// Count a round at the current time; `false`, with the run failed, once a
/// time step has spent [`ROUNDS_AT_TIME`] of them.
fn charge_round() -> bool {
    let exceeded = with_host(|host| {
        borrow_cell_mut(&host.callbacks, |state| {
            state.rounds_at_time += 1;
            (state.rounds_at_time > ROUNDS_AT_TIME).then_some(state.time)
        })
    });
    if let Some(time) = exceeded {
        fail(format!(
            "simulation does not advance at time {time}: callbacks and the settles of their \
             writes repeated {ROUNDS_AT_TIME} times"
        ));
        return false;
    }
    true
}

/// Fire the ReadOnly callbacks, with VPI writes rejected (IEEE 1800-2023
/// 38.36.2).
fn fire_read_only() -> bool {
    with_host(fire_read_only_for_host)
}

fn fire_read_only_for_host(host: &super::VpiHost) -> bool {
    borrow_cell_mut(&host.callbacks, |state| state.read_only = true);
    let progressed = fire_all_for_host(host, CB_READ_ONLY_SYNCH);
    borrow_cell_mut(&host.callbacks, |state| state.read_only = false);
    progressed
}

fn current_time() -> u64 {
    with_host(|host| borrow_cell(&host.callbacks, |state| state.time))
}

/// Fire the timers due now, including the zero-delay ones they register,
/// until none is due. Returns whether any fired.
fn fire_due_timers() -> bool {
    let mut fired = false;
    loop {
        let due = due_ids();
        if due.is_empty() || finish_requested() {
            return fired;
        }
        for id in due {
            fired |= fire(id);
            if finish_requested() {
                return fired;
            }
        }
        if !charge_round() {
            return fired;
        }
    }
}

/// Whether a VPI write changed the timed simulation since the last call.
fn take_timed_writes() -> bool {
    super::with_timed_mut(super::TimedRuntime::take_wrote).unwrap_or(false)
}

/// What the regions after a slot's processes reached.
enum Regions {
    /// A callback wrote: the simulation settles the writes at the same
    /// time, and the regions start again.
    Wrote,
    /// Every region ran; the next is the next time.
    Done,
}

/// The regions of a settled time slot: value changes, then ReadWrite until
/// the callbacks write nothing, then ReadOnly (IEEE 1800-2023 4.4.2).
fn settled_regions() -> Regions {
    take_timed_writes();
    loop {
        if finish_requested() || !charge_round() {
            return Regions::Done;
        }
        if dispatch_value_changes() {
            if take_timed_writes() {
                return Regions::Wrote;
            }
            continue;
        }
        // A zero-delay timer that a callback of this time step registered
        // runs before the step closes.
        if fire_due_timers() {
            if take_timed_writes() {
                return Regions::Wrote;
            }
            continue;
        }
        if super::flush_inertial_writes() {
            return Regions::Wrote;
        }
        let read_write = registration_ids(CB_READ_WRITE_SYNCH);
        if read_write.is_empty() {
            break;
        }
        for id in read_write {
            fire(id);
            if finish_requested() {
                return Regions::Done;
            }
        }
        if take_timed_writes() {
            return Regions::Wrote;
        }
    }
    fire_read_only();
    with_host(|host| {
        borrow_cell_mut(&host.callbacks, |state| {
            state.read_only_at = Some(state.time)
        })
    });
    Regions::Done
}

/// Shows a timed simulation's slots to the VPI callbacks: value changes at
/// every wave boundary, so that an edge is seen before the domains it
/// triggers commit, and the regions of each settled slot.
struct TimedObserver;

impl celox::SlotObserver<celox::NativeBackend> for TimedObserver {
    fn before_domains(&mut self, io: &mut celox::SlotIo<'_, celox::NativeBackend>, time: u64) {
        in_slot(io, time, || {
            dispatch_value_changes();
        });
    }

    fn end_of_slot(&mut self, io: &mut celox::SlotIo<'_, celox::NativeBackend>, time: u64) {
        in_slot(io, time, || {
            settled_regions();
        });
    }
}

/// Run `regions` with the VPI entry points reaching the state through `io`.
fn in_slot(io: &mut celox::SlotIo<'_, celox::NativeBackend>, time: u64, regions: impl FnOnce()) {
    set_time(time);
    // Safety: `io` outlives the dispatch below and nothing else uses it until
    // `leave_slot`.
    super::with_timed_mut(|timed| unsafe { timed.enter_slot(io) });
    regions();
    super::with_timed_mut(super::TimedRuntime::leave_slot);
    // vpiFinish takes effect when the application callback returns
    // (IEEE 1800-2023 38.4), before the scheduler runs more DUT work.
    if finish_requested() {
        io.request_finish();
    }
}

/// Run `step` on the timed simulation, out of the session while it runs so
/// that its observer's callbacks can reach the VPI entry points.
fn timed_step(
    step: impl FnOnce(
        &mut celox::Simulation<celox::NativeBackend>,
    ) -> Result<(), celox::RuntimeErrorCode>,
) {
    let watched = watched_signals();
    let Some(mut simulation) = super::with_timed_mut(|timed| timed.simulation.take()).flatten()
    else {
        return;
    };
    simulation.set_observed_signals(&watched);
    let result = step(&mut simulation);
    let ended = simulation.is_finished() || simulation.is_stopped();
    super::with_timed_mut(|timed| timed.simulation = Some(simulation));
    super::process_runtime_events();
    if let Err(error) = result {
        fail(format!("simulation failed: {error}"));
    } else if ended {
        // `$stop` ends a run that has no interactive mode, as `vpiStop` does.
        request_finish();
    }
}

fn with_simulation<R>(
    operation: impl FnOnce(&mut celox::Simulation<celox::NativeBackend>) -> R,
) -> Option<R> {
    super::with_timed_mut(|timed| timed.simulation.as_mut().map(operation)).flatten()
}

/// The VPI regions of every time slot of a timed simulation, until
/// `vpiFinish`, the design's `$finish`, or no activity remains.
pub(super) fn run_timed() -> bool {
    with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.running = true));
    if let Some(Some(mut simulation)) = super::with_timed_mut(|timed| timed.simulation.take()) {
        simulation.observe_slots(Box::new(TimedObserver), &[]);
        super::with_timed_mut(|timed| timed.simulation = Some(simulation));
    }
    fire_all(CB_START_OF_SIMULATION);
    while !finish_requested() && charge_round() {
        let due = with_host(|host| {
            borrow_cell(&host.callbacks, |state| {
                state
                    .callbacks
                    .values()
                    .filter_map(|registration| registration.due)
                    .min()
            })
        });
        let event = with_simulation(|simulation| simulation.next_event_time()).flatten();
        let inertial = super::has_inertial_writes().then(current_time);
        let Some(time) = due.into_iter().chain(event).chain(inertial).min() else {
            break;
        };
        if time > current_time() {
            timed_step(|simulation| simulation.advance_to(time));
            if finish_requested() {
                break;
            }
            set_time(time);
            fire_all(CB_NEXT_SIM_TIME);
        } else if with_host(|host| borrow_cell(&host.callbacks, |state| state.read_only_at))
            == Some(time)
            && event != Some(time)
            && inertial != Some(time)
        {
            // A zero-delay timer that a ReadOnly callback registered runs,
            // still read-only: the time step's regions do not open again.
            with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.read_only = true));
            for id in due_ids() {
                fire(id);
            }
            with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.read_only = false));
            continue;
        }
        fire_due_timers();
        // A finish from a timer ends the run before the design's slot.
        if finish_requested() {
            break;
        }
        with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.read_only_at = None));
        timed_step(|simulation| simulation.run_until(time));
        // The slot ran no regions when nothing but callbacks happened at
        // this time: run them here, settling what they write.
        while !finish_requested()
            && with_host(|host| borrow_cell(&host.callbacks, |state| state.read_only_at))
                != Some(time)
        {
            timed_step(|simulation| simulation.eval_comb());
            if finish_requested() {
                break;
            }
            take_timed_writes();
            if let Regions::Done = settled_regions() {
                break;
            }
            timed_step(|simulation| simulation.run_until(time));
        }
    }
    let finished = finish_requested();
    for id in registration_ids(CB_END_OF_SIMULATION) {
        fire(id);
    }
    with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.running = false));
    finished
}

pub(super) fn is_running() -> bool {
    with_host(is_running_for_host)
}

pub(super) fn is_running_for_host(host: &super::VpiHost) -> bool {
    borrow_cell(&host.callbacks, |state| state.running)
}

pub(super) fn finish_requested() -> bool {
    with_host(finish_requested_for_host)
}

pub(super) fn finish_requested_for_host(host: &super::VpiHost) -> bool {
    borrow_cell(&host.callbacks, |state| state.finish)
}

pub(super) fn request_finish() {
    with_host(request_finish_for_host);
}

pub(super) fn request_finish_for_host(host: &super::VpiHost) {
    borrow_cell_mut(&host.callbacks, |state| state.finish = true)
}

pub(super) fn fail(message: String) {
    with_host(|host| fail_for_host(host, message));
}

pub(super) fn fail_for_host(host: &super::VpiHost, message: String) {
    borrow_cell_mut(&host.callbacks, |state| {
        state.error.get_or_insert(message);
        state.finish = true;
    });
}

pub(super) fn has_error_for_host(host: &super::VpiHost) -> bool {
    borrow_cell(&host.callbacks, |state| state.error.is_some())
}

pub(super) fn take_error() -> Option<String> {
    with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.error.take()))
}

pub(super) fn remove(id: u64, handle: VpiHandle) -> i32 {
    let queued = with_host(|host| {
        borrow_cell_mut(&host.callbacks, |state| {
            state.callbacks.remove(&id).is_some()
        })
    });
    if queued {
        // Safety: removing a queued callback returns ownership of its handle.
        drop(unsafe { Box::from_raw(handle) });
        return 1;
    }
    let firing = with_host(|host| {
        borrow_cell_mut(&host.callbacks, |state| {
            if let Some(cancelled) = state.firing.get_mut(&id) {
                *cancelled = true;
                true
            } else {
                false
            }
        })
    });
    i32::from(firing)
}

#[unsafe(no_mangle)]
/// Register one cocotb-supported VPI callback.
///
/// # Safety
///
/// `data` and its selected pointers must satisfy the VPI callback ABI.
pub unsafe extern "C" fn vpi_register_cb(data: *const VpiCbData) -> VpiHandle {
    let Some(host) = super::selected_host() else {
        return ptr::null_mut();
    };
    if data.is_null() {
        return ptr::null_mut();
    }
    // Safety: checked above and copied before returning.
    let mut data = unsafe { *data };
    if data.cb_rtn.is_none()
        || !matches!(
            data.reason,
            CB_VALUE_CHANGE
                | CB_READ_WRITE_SYNCH
                | CB_READ_ONLY_SYNCH
                | CB_NEXT_SIM_TIME
                | CB_AFTER_DELAY
                | CB_START_OF_SIMULATION
                | CB_END_OF_SIMULATION
        )
    {
        return ptr::null_mut();
    }
    let signal = callback_signal(&data);
    if data.reason == CB_VALUE_CHANGE && signal.is_none() {
        return ptr::null_mut();
    }
    // VPI registration copies the selected records. Their original storage is
    // commonly stack-local and need not outlive this call.
    let mut time_storage = if data.time.is_null() {
        None
    } else {
        // Safety: the registration contract makes the pointed record readable
        // for the duration of this call.
        Some(Box::new(unsafe { *data.time }))
    };
    let mut value_storage = if data.value.is_null() {
        None
    } else {
        // Safety: copy the active request record while registration storage is live.
        let value = unsafe { &*data.value };
        Some(Box::new(VpiValue {
            format: value.format,
            value: value.value,
        }))
    };
    data.time = time_storage
        .as_deref_mut()
        .map_or(ptr::null_mut(), std::ptr::from_mut);
    data.value = value_storage
        .as_deref_mut()
        .map_or(ptr::null_mut(), std::ptr::from_mut);
    borrow_cell_mut(&host.callbacks, |state| {
        let id = state.next_id;
        state.next_id += 1;
        let handle = new_callback_handle(id);
        let due = (data.reason == CB_AFTER_DELAY)
            .then(|| state.time.saturating_add(callback_delay(&data)));
        let snapshot = signal.and_then(|signal| {
            super::value_snapshot_at(&host, signal).map(|(snapshot, _)| snapshot.canonical())
        });
        state.callbacks.insert(
            id,
            Registration {
                data,
                signal,
                _time_storage: time_storage,
                _value_storage: value_storage,
                handle,
                due,
                snapshot,
            },
        );
        handle
    })
}

#[unsafe(no_mangle)]
/// Remove a callback registration.
///
/// # Safety
///
/// `reference` must be a live callback handle returned by [`vpi_register_cb`].
pub unsafe extern "C" fn vpi_remove_cb(reference: VpiHandle) -> i32 {
    // Safety: reference follows the VPI handle contract.
    let Some(handle) = (unsafe { handle_mut(reference) }) else {
        return 0;
    };
    let HandleKind::Callback(id) = handle.kind else {
        return 0;
    };
    remove(id, reference)
}

#[unsafe(no_mangle)]
/// Return the current simulation time.
///
/// # Safety
///
/// `time` must point to writable [`VpiTime`] storage.
pub unsafe extern "C" fn vpi_get_time(_object: VpiHandle, time: *mut VpiTime) {
    if time.is_null() {
        return;
    }
    let now = with_host(|host| borrow_cell(&host.callbacks, |state| state.time));
    // Safety: checked non-null above.
    let time = unsafe { &mut *time };
    if time.type_ == VPI_SCALED_REAL_TIME {
        time.high = 0;
        time.low = 0;
        time.real = now as f64;
    } else {
        time.high = (now >> 32) as u32;
        time.low = now as u32;
        time.real = 0.0;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn vpi_control(operation: i32) -> i32 {
    if matches!(operation, VPI_STOP | VPI_FINISH) {
        with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.finish = true));
        1
    } else {
        0
    }
}

#[unsafe(no_mangle)]
/// Return process arguments and Celox product information.
///
/// # Safety
///
/// `info` must point to writable [`VpiVlogInfo`] storage.
#[allow(clippy::disallowed_methods)] // VPI requires exposing the process argv.
pub unsafe extern "C" fn vpi_get_vlog_info(info: *mut VpiVlogInfo) -> i32 {
    if info.is_null() || !host_is_active() {
        return 0;
    }
    with_host(|host| {
        borrow_cell_mut(&host.vlog, |storage| {
            let storage = storage.get_or_insert_with(|| {
                let args = std::env::args_os()
                    .map(|arg| CString::new(arg.to_string_lossy().replace('\0', "")).unwrap())
                    .collect::<Vec<_>>();
                let arg_ptrs = args.iter().map(|arg| arg.as_ptr().cast_mut()).collect();
                VlogStorage {
                    _args: args,
                    arg_ptrs,
                }
            });
            // Safety: checked non-null above; storage lives in the active session.
            let info = unsafe { &mut *info };
            info.argc = i32::try_from(storage.arg_ptrs.len()).unwrap_or(i32::MAX);
            info.argv = storage.arg_ptrs.as_mut_ptr();
            info.product = c"Celox".as_ptr().cast_mut();
            info.version = c"0.0.0".as_ptr().cast_mut();
            1
        })
    })
}

#[unsafe(no_mangle)]
/// Report the last VPI error. Celox clears errors at each ABI boundary, so the
/// initial compatibility layer currently reports no pending error.
///
/// # Safety
///
/// A non-null `info` must point to writable [`VpiErrorInfo`] storage.
pub unsafe extern "C" fn vpi_chk_error(_info: *mut VpiErrorInfo) -> i32 {
    0
}

#[unsafe(no_mangle)]
/// Resolve an indexed child. Packed bit handles are not yet materialized.
///
/// # Safety
///
/// A non-null `reference` must be a live handle returned by this library.
pub unsafe extern "C" fn vpi_handle_by_index(reference: VpiHandle, index: i32) -> VpiHandle {
    // Safety: forwarded from the caller under the VPI handle contract.
    let Some(ObjectRef::Signal(selection)) = (unsafe { object_ref(reference) }) else {
        return ptr::null_mut();
    };
    super::with_runtime(|runtime| selection.select(runtime, index))
        .flatten()
        .map_or(ptr::null_mut(), |selection| {
            super::new_object_handle(ObjectRef::Signal(selection))
        })
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_current_time() -> u64 {
    with_host(current_time_for_host)
}

pub(super) fn current_time_for_host(host: &super::VpiHost) -> u64 {
    borrow_cell(&host.callbacks, |state| state.time)
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_runtime_cookie() -> *mut c_void {
    ptr::null_mut()
}

#[cfg(test)]
mod snapshot_tests {
    use super::ValueSnapshot;
    use celox::BigUint;

    #[test]
    fn canonical_snapshots_compare_growing_values_and_mask_only_changes() {
        for bit in [0usize, 31, 32, 63, 64, 65, 127, 128] {
            let value = BigUint::from(1u8) << bit;
            let known = ValueSnapshot::new(value.clone(), BigUint::default());
            let unknown = ValueSnapshot::new(value.clone(), value.clone());
            assert_ne!(known, unknown);
            assert_ne!(
                known,
                ValueSnapshot::new(BigUint::default(), BigUint::default())
            );
            assert_eq!(
                known,
                ValueSnapshot::Wide(value.clone(), BigUint::default()).canonical()
            );
            assert_eq!(
                unknown,
                ValueSnapshot::Wide(value.clone(), value.clone()).canonical()
            );
            if bit < 64 {
                assert_eq!(known, ValueSnapshot::Word(1u64 << bit, 0));
            } else {
                assert!(matches!(known, ValueSnapshot::Wide(..)));
            }
        }
    }
}
