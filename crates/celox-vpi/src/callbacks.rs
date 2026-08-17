use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{CString, c_char, c_void},
    ptr,
};

use fxhash::FxHashMap as HashMap;

use super::{
    HandleKind, ObjectRef, VpiHandle, VpiValue, handle_mut, new_callback_handle, object_ref,
    value_bits, vpi_get_value,
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

struct Registration {
    data: VpiCbData,
    handle: VpiHandle,
    due: Option<u64>,
    snapshot: Option<(celox::BigUint, celox::BigUint)>,
}

#[derive(Default)]
struct CallbackState {
    next_id: u64,
    callbacks: BTreeMap<u64, Registration>,
    firing: HashMap<u64, bool>,
    time: u64,
    finish: bool,
    running: bool,
    error: Option<String>,
}

struct VlogStorage {
    _args: Vec<CString>,
    arg_ptrs: Vec<*mut c_char>,
}

thread_local! {
    static STATE: RefCell<CallbackState> = RefCell::new(CallbackState::default());
    static VLOG: RefCell<Option<VlogStorage>> = const { RefCell::new(None) };
}

pub(super) fn reset() {
    let old = STATE.with_borrow_mut(std::mem::take);
    for registration in old.callbacks.into_values() {
        // Safety: reset owns every queued callback handle. It is only called
        // outside callback execution while replacing the runtime instance.
        drop(unsafe { Box::from_raw(registration.handle) });
    }
    VLOG.with_borrow_mut(|vlog| *vlog = None);
}

fn callback_signal(data: &VpiCbData) -> Option<celox::ReflectionSignalId> {
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
    STATE.with_borrow(|state| {
        state
            .callbacks
            .iter()
            .filter_map(|(&id, registration)| (registration.data.reason == reason).then_some(id))
            .collect()
    })
}

fn changed_value_ids() -> Vec<u64> {
    STATE.with_borrow(|state| {
        state
            .callbacks
            .iter()
            .filter_map(|(&id, registration)| {
                let signal = callback_signal(&registration.data)?;
                let (value, mask, _) = value_bits(signal)?;
                (registration.snapshot.as_ref() != Some(&(value, mask))).then_some(id)
            })
            .collect()
    })
}

fn due_ids() -> Vec<u64> {
    STATE.with_borrow(|state| {
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
    let Some(mut registration) = STATE.with_borrow_mut(|state| {
        let registration = state.callbacks.remove(&id)?;
        state.firing.insert(id, false);
        Some(registration)
    }) else {
        return false;
    };

    let now = STATE.with_borrow(|state| state.time);
    let delivered_snapshot = callback_signal(&registration.data)
        .and_then(|signal| value_bits(signal).map(|(value, mask, _)| (value, mask)));
    if !registration.data.time.is_null() {
        // Safety: cocotb stores callback time in its live callback object.
        let time = unsafe { &mut *registration.data.time };
        if time.type_ != VPI_SUPPRESS_TIME {
            time.type_ = VPI_SIM_TIME;
            time.high = (now >> 32) as u32;
            time.low = now as u32;
            time.real = 0.0;
        }
    }

    if registration.data.reason == CB_VALUE_CHANGE && !registration.data.value.is_null() {
        // Safety: callback registration keeps both pointers live until the
        // callback is removed, and vpi_get_value writes the requested format.
        unsafe { vpi_get_value(registration.data.obj, registration.data.value) };
    }

    if let Some(callback) = registration.data.cb_rtn {
        // Safety: the function and callback storage were supplied by the VPI
        // module and remain live until it removes or fires the registration.
        unsafe { callback(&mut registration.data) };
    }

    let cancelled = STATE.with_borrow_mut(|state| state.firing.remove(&id).unwrap_or(true));
    if registration.data.reason == CB_VALUE_CHANGE && !cancelled {
        registration.snapshot = delivered_snapshot;
        STATE.with_borrow_mut(|state| {
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
    let ids = registration_ids(reason);
    let progressed = !ids.is_empty();
    for id in ids {
        fire(id);
    }
    progressed
}

pub(super) fn run() -> bool {
    STATE.with_borrow_mut(|state| state.running = true);
    fire_all(CB_START_OF_SIMULATION);
    let mut iterations = 0usize;
    'scheduler: loop {
        iterations += 1;
        if iterations > 1_000_000 {
            fail("VPI callback scheduler exceeded 1000000 iterations".to_string());
            break;
        }

        let mut progressed = false;
        for id in due_ids() {
            progressed |= fire(id);
        }
        if !super::flush_pending_writes() {
            break;
        }
        loop {
            let changed = changed_value_ids();
            if !changed.is_empty() {
                for id in changed {
                    progressed |= fire(id);
                }
                if !super::flush_pending_writes() {
                    break 'scheduler;
                }
                continue;
            }
            let read_write = registration_ids(CB_READ_WRITE_SYNCH);
            if !read_write.is_empty() {
                for id in read_write {
                    progressed |= fire(id);
                }
                if !super::flush_pending_writes() {
                    break 'scheduler;
                }
                continue;
            }
            break;
        }
        progressed |= fire_all(CB_READ_ONLY_SYNCH);

        if STATE.with_borrow(|state| state.finish) {
            break;
        }
        if progressed {
            continue;
        }

        let next_time = STATE.with_borrow(|state| {
            state
                .callbacks
                .values()
                .filter_map(|registration| registration.due)
                .min()
        });
        let Some(next_time) = next_time else {
            break;
        };
        STATE.with_borrow_mut(|state| state.time = next_time);
        fire_all(CB_NEXT_SIM_TIME);
    }
    let finished = STATE.with_borrow(|state| state.finish);
    fire_all(CB_END_OF_SIMULATION);
    STATE.with_borrow_mut(|state| state.running = false);
    finished
}

pub(super) fn is_running() -> bool {
    STATE.with_borrow(|state| state.running)
}

pub(super) fn fail(message: String) {
    STATE.with_borrow_mut(|state| {
        state.error.get_or_insert(message);
        state.finish = true;
    });
}

pub(super) fn has_error() -> bool {
    STATE.with_borrow(|state| state.error.is_some())
}

pub(super) fn take_error() -> Option<String> {
    STATE.with_borrow_mut(|state| state.error.take())
}

pub(super) fn remove(id: u64, handle: VpiHandle) -> i32 {
    let queued = STATE.with_borrow_mut(|state| state.callbacks.remove(&id).is_some());
    if queued {
        // Safety: removing a queued callback returns ownership of its handle.
        drop(unsafe { Box::from_raw(handle) });
        return 1;
    }
    let firing = STATE.with_borrow_mut(|state| {
        if let Some(cancelled) = state.firing.get_mut(&id) {
            *cancelled = true;
            true
        } else {
            false
        }
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
    if data.is_null() {
        return ptr::null_mut();
    }
    // Safety: checked above and copied before returning.
    let data = unsafe { *data };
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
    STATE.with_borrow_mut(|state| {
        let id = state.next_id;
        state.next_id += 1;
        let handle = new_callback_handle(id);
        let due = (data.reason == CB_AFTER_DELAY)
            .then(|| state.time.saturating_add(callback_delay(&data)));
        let snapshot =
            signal.and_then(|signal| value_bits(signal).map(|(value, mask, _)| (value, mask)));
        state.callbacks.insert(
            id,
            Registration {
                data,
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
    let now = STATE.with_borrow(|state| state.time);
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
        STATE.with_borrow_mut(|state| state.finish = true);
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
    if info.is_null() {
        return 0;
    }
    VLOG.with_borrow_mut(|storage| {
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
        // Safety: checked non-null above; storage lives in thread-local state.
        let info = unsafe { &mut *info };
        info.argc = i32::try_from(storage.arg_ptrs.len()).unwrap_or(i32::MAX);
        info.argv = storage.arg_ptrs.as_mut_ptr();
        info.product = c"Celox".as_ptr().cast_mut();
        info.version = c"0.0.0".as_ptr().cast_mut();
        1
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
pub unsafe extern "C" fn vpi_handle_by_index(_reference: VpiHandle, _index: i32) -> VpiHandle {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_current_time() -> u64 {
    STATE.with_borrow(|state| state.time)
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_runtime_cookie() -> *mut c_void {
    ptr::null_mut()
}
