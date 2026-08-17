//! Minimal IEEE VPI compatibility layer backed by an attached Celox image.
//!
//! This first layer intentionally implements object discovery, hierarchy
//! iteration, properties, and immediate signal values. Callback scheduling is
//! kept separate because it must share the runtime's simulation-region loop.

#![cfg_attr(
    not(any(target_arch = "x86_64", target_arch = "aarch64")),
    allow(dead_code)
)]

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("celox-vpi currently supports only x86-64 and AArch64");

use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char, c_void},
    io::Write,
    ptr,
};

use celox::{
    BigUint, DomainKind, NativeProgramInstance, NativeSignalIdentity, ReflectionScopeId,
    ReflectionSignalId, RuntimeEvent, RuntimeFormatContext, SignalDirection, SimBackend,
};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

mod callbacks;
pub use callbacks::{
    CB_AFTER_DELAY, CB_END_OF_SIMULATION, CB_NEXT_SIM_TIME, CB_READ_ONLY_SYNCH,
    CB_READ_WRITE_SYNCH, CB_START_OF_SIMULATION, CB_VALUE_CHANGE, VPI_SCALED_REAL_TIME,
    VPI_SIM_TIME, VPI_SUPPRESS_TIME, VpiCbData, VpiErrorInfo, VpiTime, VpiVlogInfo,
    celox_vpi_current_time, vpi_chk_error, vpi_control, vpi_get_time, vpi_get_vlog_info,
    vpi_handle_by_index, vpi_register_cb, vpi_remove_cb,
};

pub const VPI_BIN_STR_VAL: i32 = 1;
pub const VPI_HEX_STR_VAL: i32 = 4;
pub const VPI_SCALAR_VAL: i32 = 5;
pub const VPI_INT_VAL: i32 = 6;
pub const VPI_VECTOR_VAL: i32 = 9;
pub const VPI_SUPPRESS_VAL: i32 = 13;

pub const VPI_0: i32 = 0;
pub const VPI_1: i32 = 1;
pub const VPI_Z: i32 = 2;
pub const VPI_X: i32 = 3;

pub const VPI_ITERATOR: i32 = 27;
pub const VPI_CONSTANT: i32 = 7;
pub const VPI_MODULE: i32 = 32;
pub const VPI_NET: i32 = 36;
pub const VPI_PORT: i32 = 44;
pub const VPI_REG: i32 = 48;
pub const VPI_LEFT_RANGE: i32 = 79;
pub const VPI_PARENT: i32 = 81;
pub const VPI_RIGHT_RANGE: i32 = 83;
pub const VPI_SCOPE: i32 = 84;
pub const VPI_INTERNAL_SCOPE: i32 = 92;
pub const VPI_VARIABLES: i32 = 100;
pub const VPI_INSTANCE: i32 = 745;

pub const VPI_TYPE: i32 = 1;
pub const VPI_NAME: i32 = 2;
pub const VPI_FULL_NAME: i32 = 3;
pub const VPI_SIZE: i32 = 4;
pub const VPI_TOP_MODULE: i32 = 7;
pub const VPI_DEF_NAME: i32 = 9;
pub const VPI_SCALAR: i32 = 17;
pub const VPI_VECTOR: i32 = 18;
pub const VPI_DIRECTION: i32 = 20;
pub const VPI_SIGNED: i32 = 65;
pub const VPI_TIME_UNIT: i32 = 11;
pub const VPI_TIME_PRECISION: i32 = 12;

pub const VPI_INPUT: i32 = 1;
pub const VPI_OUTPUT: i32 = 2;
pub const VPI_INOUT: i32 = 3;
pub const VPI_NO_DIRECTION: i32 = 5;

pub const VPI_NO_DELAY: i32 = 1;
pub const VPI_INERTIAL_DELAY: i32 = 2;
pub const VPI_FORCE_FLAG: i32 = 5;
pub const VPI_RELEASE_FLAG: i32 = 6;
pub const VPI_FINISH: i32 = 67;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VpiVecVal {
    pub aval: i32,
    pub bval: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union VpiValueData {
    pub str_: *mut c_char,
    pub scalar: i32,
    pub integer: i32,
    pub real: f64,
    pub time: *mut c_void,
    pub vector: *mut VpiVecVal,
    pub misc: *mut c_char,
}

#[repr(C)]
pub struct VpiValue {
    pub format: i32,
    pub value: VpiValueData,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectRef {
    Scope(ReflectionScopeId),
    Signal(ReflectionSignalId),
}

#[derive(Debug)]
enum HandleKind {
    Object(ObjectRef),
    Constant(i32),
    Callback(u64),
    Iterator {
        objects: Vec<ObjectRef>,
        next: usize,
    },
}

/// Opaque allocation referenced by the C `vpiHandle` pointer type.
pub struct VpiHandleObject {
    kind: HandleKind,
    name: CString,
    full_name: CString,
    value_string: Option<CString>,
    value_vector: Vec<VpiVecVal>,
}

pub type VpiHandle = *mut VpiHandleObject;

thread_local! {
    static RUNTIME: RefCell<Option<NativeProgramInstance>> = const { RefCell::new(None) };
    static FORCED_VALUES: RefCell<HashMap<NativeSignalIdentity, ForcedValue>> = RefCell::new(HashMap::default());
    static PENDING_WRITES: RefCell<PendingWrites> = RefCell::new(PendingWrites::default());
}

#[derive(Clone)]
struct ForcedValue {
    value: BigUint,
    mask: BigUint,
    deposited_value: BigUint,
    deposited_mask: BigUint,
}

#[derive(Default)]
struct PendingWrites {
    deposits: HashMap<NativeSignalIdentity, PendingDeposit>,
    edge_batches: Vec<PendingEdgeBatch>,
    open_edge_batch: Option<usize>,
    settle: bool,
}

struct PendingEdgeBatch {
    active_signals: Vec<NativeSignalIdentity>,
    transition_signals: Vec<NativeSignalIdentity>,
    deposits: HashMap<NativeSignalIdentity, PendingDeposit>,
}

#[derive(Clone)]
struct PendingDeposit {
    id: ReflectionSignalId,
    value: BigUint,
    mask: BigUint,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LogicLevel {
    Zero,
    One,
    X,
    Z,
}

fn logic_level(value: &BigUint, mask: &BigUint) -> LogicLevel {
    match (value.bit(0), mask.bit(0)) {
        (false, false) => LogicLevel::Zero,
        (true, false) => LogicLevel::One,
        (true, true) => LogicLevel::X,
        (false, true) => LogicLevel::Z,
    }
}

fn is_active_edge(kind: DomainKind, old: LogicLevel, new: LogicLevel) -> bool {
    match kind {
        DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh => {
            matches!(old, LogicLevel::Zero) && !matches!(new, LogicLevel::Zero)
                || matches!(old, LogicLevel::X | LogicLevel::Z) && matches!(new, LogicLevel::One)
        }
        DomainKind::ClockNegedge | DomainKind::ResetAsyncLow => {
            matches!(old, LogicLevel::One) && !matches!(new, LogicLevel::One)
                || matches!(old, LogicLevel::X | LogicLevel::Z) && matches!(new, LogicLevel::Zero)
        }
        DomainKind::Other => false,
    }
}

fn record_pending_write(
    pending: &mut PendingWrites,
    id: ReflectionSignalId,
    identity: NativeSignalIdentity,
    value: BigUint,
    mask: BigUint,
    domain_kind: DomainKind,
    old_level: LogicLevel,
    new_level: LogicLevel,
) {
    let active = is_active_edge(domain_kind, old_level, new_level);
    let level_changed = old_level != new_level;
    let event_transition = domain_kind != DomainKind::Other && level_changed;
    let open_contains_signal = pending.open_edge_batch.is_some_and(|index| {
        pending.edge_batches[index]
            .transition_signals
            .contains(&identity)
    });

    if open_contains_signal && event_transition {
        pending.open_edge_batch = None;
    }

    pending
        .deposits
        .insert(identity, PendingDeposit { id, value, mask });
    if event_transition {
        if let Some(index) = pending.open_edge_batch {
            pending.edge_batches[index]
                .transition_signals
                .push(identity);
            if active {
                pending.edge_batches[index].active_signals.push(identity);
            }
            pending.edge_batches[index].deposits = pending.deposits.clone();
        } else {
            pending.edge_batches.push(PendingEdgeBatch {
                active_signals: active.then_some(identity).into_iter().collect(),
                transition_signals: vec![identity],
                deposits: pending.deposits.clone(),
            });
            pending.open_edge_batch = Some(pending.edge_batches.len() - 1);
        }
    } else if let Some(index) = pending.open_edge_batch {
        pending.edge_batches[index].deposits = pending.deposits.clone();
    }
    pending.settle = true;
}

fn apply_pending_deposits(
    runtime: &mut NativeProgramInstance,
    deposits: &HashMap<NativeSignalIdentity, PendingDeposit>,
) {
    let writes = deposits
        .values()
        .filter_map(|deposit| {
            runtime
                .reflection()
                .signal(deposit.id)
                .map(|signal| (signal.signal, deposit.value.clone(), deposit.mask.clone()))
        })
        .collect::<Vec<_>>();
    for (signal, value, mask) in writes {
        runtime.backend_mut().set_four_state(signal, value, mask);
    }
}

/// Install an instance explicitly. This is primarily useful to an embedding
/// runtime which has already selected the attached image.
pub fn install_runtime(instance: NativeProgramInstance) {
    callbacks::reset();
    FORCED_VALUES.with_borrow_mut(HashMap::clear);
    PENDING_WRITES.with_borrow_mut(|pending| *pending = PendingWrites::default());
    RUNTIME.with_borrow_mut(|runtime| *runtime = Some(instance));
}

pub fn clear_runtime() {
    RUNTIME.with_borrow_mut(|runtime| *runtime = None);
    FORCED_VALUES.with_borrow_mut(HashMap::clear);
    PENDING_WRITES.with_borrow_mut(|pending| *pending = PendingWrites::default());
    callbacks::reset();
}

/// Run VPI simulation regions until `vpiFinish` or until no future activity
/// remains. Returns true when the test requested a normal finish.
pub fn run_callbacks() -> bool {
    run_callbacks_result().unwrap_or(false)
}

/// Run callbacks while preserving fatal DUT diagnostics for executable hosts.
pub fn run_callbacks_result() -> Result<bool, String> {
    process_runtime_events();
    if let Some(error) = callbacks::take_error() {
        return Err(error);
    }
    let finished = callbacks::run();
    if let Some(error) = callbacks::take_error() {
        Err(error)
    } else {
        Ok(finished)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_load_current_executable() -> i32 {
    // Safety: the VPI host only loads the image attached to its own trusted
    // executable artifact.
    match unsafe { NativeProgramInstance::from_current_executable() } {
        Ok(instance) => {
            install_runtime(instance);
            1
        }
        Err(_) => 0,
    }
}

fn with_runtime<R>(operation: impl FnOnce(&NativeProgramInstance) -> R) -> Option<R> {
    RUNTIME.with_borrow(|runtime| runtime.as_ref().map(operation))
}

fn with_runtime_mut<R>(operation: impl FnOnce(&mut NativeProgramInstance) -> R) -> Option<R> {
    RUNTIME.with_borrow_mut(|runtime| runtime.as_mut().map(operation))
}

fn c_string(value: &str) -> CString {
    CString::new(value.replace('\0', "")).expect("NUL bytes were removed")
}

fn new_object_handle(object: ObjectRef) -> VpiHandle {
    let (name, full_name) = with_runtime(|runtime| match object {
        ObjectRef::Scope(id) => runtime
            .reflection()
            .scope(id)
            .map(|scope| (scope.name.clone(), scope.full_name.clone())),
        ObjectRef::Signal(id) => runtime
            .reflection()
            .signal(id)
            .map(|signal| (signal.name.clone(), signal.full_name.clone())),
    })
    .flatten()
    .unwrap_or_default();
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Object(object),
        name: c_string(&name),
        full_name: c_string(&full_name),
        value_string: None,
        value_vector: Vec::new(),
    }))
}

fn new_iterator_handle(objects: Vec<ObjectRef>) -> VpiHandle {
    if objects.is_empty() {
        return ptr::null_mut();
    }
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Iterator { objects, next: 0 },
        name: c_string(""),
        full_name: c_string(""),
        value_string: None,
        value_vector: Vec::new(),
    }))
}

fn new_constant_handle(value: i32) -> VpiHandle {
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Constant(value),
        name: c_string(""),
        full_name: c_string(""),
        value_string: None,
        value_vector: Vec::new(),
    }))
}

fn new_callback_handle(id: u64) -> VpiHandle {
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Callback(id),
        name: c_string(""),
        full_name: c_string(""),
        value_string: None,
        value_vector: Vec::new(),
    }))
}

unsafe fn handle_mut<'a>(handle: VpiHandle) -> Option<&'a mut VpiHandleObject> {
    if handle.is_null() {
        None
    } else {
        // Safety: VPI handles are pointers returned by `Box::into_raw` in this
        // module and remain live until `vpi_free_object` consumes them.
        Some(unsafe { &mut *handle })
    }
}

unsafe fn object_ref(handle: VpiHandle) -> Option<ObjectRef> {
    // Safety: forwarded from the caller under the same VPI handle contract.
    match &unsafe { handle_mut(handle) }?.kind {
        HandleKind::Object(object) => Some(*object),
        HandleKind::Constant(_) | HandleKind::Callback(_) | HandleKind::Iterator { .. } => None,
    }
}

fn object_by_full_name(name: &str) -> Option<ObjectRef> {
    with_runtime(|runtime| {
        runtime
            .reflection()
            .scope_by_name(name)
            .map(|(id, _)| ObjectRef::Scope(id))
            .or_else(|| {
                runtime
                    .reflection()
                    .signal_by_name(name)
                    .map(|(id, _)| ObjectRef::Signal(id))
            })
    })
    .flatten()
}

#[unsafe(no_mangle)]
/// Resolve an object name using the installed runtime.
///
/// # Safety
///
/// `name` must reference a valid NUL-terminated string. A non-null `scope`
/// must be a live handle returned by this library.
pub unsafe extern "C" fn vpi_handle_by_name(name: *const c_char, scope: VpiHandle) -> VpiHandle {
    if name.is_null() {
        return ptr::null_mut();
    }
    // Safety: the VPI caller promises a valid NUL-terminated name.
    let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else {
        return ptr::null_mut();
    };
    let full_name = if scope.is_null() {
        name.to_string()
    } else {
        // Safety: scope is an opaque handle supplied by the same caller.
        let Some(ObjectRef::Scope(id)) = (unsafe { object_ref(scope) }) else {
            return ptr::null_mut();
        };
        let Some(prefix) = with_runtime(|runtime| {
            runtime
                .reflection()
                .scope(id)
                .map(|scope| scope.full_name.clone())
        })
        .flatten() else {
            return ptr::null_mut();
        };
        format!("{prefix}.{name}")
    };
    object_by_full_name(&full_name).map_or(ptr::null_mut(), new_object_handle)
}

#[unsafe(no_mangle)]
/// Resolve a relationship from an existing object.
///
/// # Safety
///
/// `reference` must be a live handle returned by this library.
pub unsafe extern "C" fn vpi_handle(kind: i32, reference: VpiHandle) -> VpiHandle {
    if matches!(kind, VPI_LEFT_RANGE | VPI_RIGHT_RANGE) {
        // Safety: reference follows the VPI handle contract.
        let Some(ObjectRef::Signal(id)) = (unsafe { object_ref(reference) }) else {
            return ptr::null_mut();
        };
        let width = with_runtime(|runtime| {
            runtime
                .reflection()
                .signal(id)
                .map(|signal| signal.signal.width)
        })
        .flatten();
        return match (kind, width) {
            (VPI_LEFT_RANGE, Some(width)) => {
                new_constant_handle(i32::try_from(width.saturating_sub(1)).unwrap_or(i32::MAX))
            }
            (VPI_RIGHT_RANGE, Some(_)) => new_constant_handle(0),
            _ => ptr::null_mut(),
        };
    }
    // Safety: reference follows the VPI handle contract.
    let Some(object) = (unsafe { object_ref(reference) }) else {
        return ptr::null_mut();
    };
    if !matches!(kind, VPI_PARENT | VPI_SCOPE) {
        return ptr::null_mut();
    }
    let parent = with_runtime(|runtime| match object {
        ObjectRef::Scope(id) => runtime
            .reflection()
            .scope(id)
            .and_then(|scope| scope.parent)
            .map(ObjectRef::Scope),
        ObjectRef::Signal(id) => runtime
            .reflection()
            .signal(id)
            .map(|signal| ObjectRef::Scope(signal.parent)),
    })
    .flatten();
    parent.map_or(ptr::null_mut(), new_object_handle)
}

#[unsafe(no_mangle)]
/// Create an iterator over objects related to `reference`.
///
/// # Safety
///
/// A non-null `reference` must be a live handle returned by this library.
pub unsafe extern "C" fn vpi_iterate(kind: i32, reference: VpiHandle) -> VpiHandle {
    let reference = if reference.is_null() {
        None
    } else {
        // Safety: reference follows the VPI handle contract.
        let Some(reference) = (unsafe { object_ref(reference) }) else {
            return ptr::null_mut();
        };
        Some(reference)
    };
    let objects = with_runtime(|runtime| {
        let reflection = runtime.reflection();
        match (kind, reference) {
            (VPI_MODULE | VPI_INSTANCE, None) => vec![ObjectRef::Scope(ReflectionScopeId(0))],
            (VPI_MODULE | VPI_INTERNAL_SCOPE, Some(ObjectRef::Scope(id))) => reflection
                .scope(id)
                .map(|scope| {
                    scope
                        .children
                        .iter()
                        .copied()
                        .map(ObjectRef::Scope)
                        .collect()
                })
                .unwrap_or_default(),
            (VPI_PORT | VPI_REG | VPI_NET | VPI_VARIABLES, Some(ObjectRef::Scope(id))) => {
                reflection
                    .scope(id)
                    .map(|scope| {
                        scope
                            .signals
                            .iter()
                            .copied()
                            .filter(|signal_id| {
                                let signal = reflection.signal(*signal_id).unwrap();
                                match kind {
                                    VPI_PORT => signal.direction != SignalDirection::Internal,
                                    VPI_REG => true,
                                    VPI_VARIABLES => signal.direction == SignalDirection::Internal,
                                    VPI_NET => false,
                                    _ => false,
                                }
                            })
                            .map(ObjectRef::Signal)
                            .collect()
                    })
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        }
    })
    .unwrap_or_default();
    new_iterator_handle(objects)
}

#[unsafe(no_mangle)]
/// Consume the next object from an iterator.
///
/// # Safety
///
/// `iterator` must be a live iterator handle returned by [`vpi_iterate`].
pub unsafe extern "C" fn vpi_scan(iterator: VpiHandle) -> VpiHandle {
    let object = {
        // Safety: iterator follows the VPI handle contract.
        let Some(iterator_ref) = (unsafe { handle_mut(iterator) }) else {
            return ptr::null_mut();
        };
        let HandleKind::Iterator { objects, next } = &mut iterator_ref.kind else {
            return ptr::null_mut();
        };
        let object = objects.get(*next).copied();
        *next += usize::from(object.is_some());
        object
    };
    let Some(object) = object else {
        // Safety: exhausted iterators are consumed by vpi_scan per the VPI
        // contract, and the borrow above ended before reclaiming the handle.
        drop(unsafe { Box::from_raw(iterator) });
        return ptr::null_mut();
    };
    new_object_handle(object)
}

fn object_type(object: ObjectRef) -> i32 {
    match object {
        ObjectRef::Scope(_) => VPI_MODULE,
        ObjectRef::Signal(_) => VPI_REG,
    }
}

#[unsafe(no_mangle)]
/// Read an integer property from an object.
///
/// # Safety
///
/// `reference` must be a live handle returned by this library.
pub unsafe extern "C" fn vpi_get(property: i32, reference: VpiHandle) -> i32 {
    if matches!(property, VPI_TIME_UNIT | VPI_TIME_PRECISION) {
        return -12;
    }
    if reference.is_null() {
        return 0;
    }
    // Safety: reference follows the VPI handle contract.
    let Some(handle) = (unsafe { handle_mut(reference) }) else {
        return 0;
    };
    if matches!(handle.kind, HandleKind::Constant(_)) {
        return if property == VPI_TYPE {
            VPI_CONSTANT
        } else {
            0
        };
    }
    // Safety: reference follows the VPI handle contract.
    let Some(object) = (unsafe { object_ref(reference) }) else {
        return 0;
    };
    match property {
        VPI_TYPE => object_type(object),
        VPI_TOP_MODULE => i32::from(object == ObjectRef::Scope(ReflectionScopeId(0))),
        VPI_SIZE | VPI_SCALAR | VPI_VECTOR | VPI_DIRECTION | VPI_SIGNED => {
            with_runtime(|runtime| {
                let ObjectRef::Signal(id) = object else {
                    return 0;
                };
                let Some(signal) = runtime.reflection().signal(id) else {
                    return 0;
                };
                match property {
                    VPI_SIZE => i32::try_from(signal.signal.width).unwrap_or(i32::MAX),
                    VPI_SCALAR => i32::from(signal.signal.width == 1),
                    VPI_VECTOR => i32::from(signal.signal.width != 1),
                    VPI_SIGNED => i32::from(signal.signed),
                    VPI_DIRECTION => match signal.direction {
                        SignalDirection::Input => VPI_INPUT,
                        SignalDirection::Output => VPI_OUTPUT,
                        SignalDirection::Inout => VPI_INOUT,
                        SignalDirection::Internal => VPI_NO_DIRECTION,
                    },
                    _ => 0,
                }
            })
            .unwrap_or(0)
        }
        _ => 0,
    }
}

#[unsafe(no_mangle)]
/// Read a string property from an object.
///
/// # Safety
///
/// `reference` must be a live handle returned by this library. The returned
/// pointer remains valid only while that handle is live and until its scratch
/// string is replaced by another operation.
pub unsafe extern "C" fn vpi_get_str(property: i32, reference: VpiHandle) -> *mut c_char {
    // Safety: reference follows the VPI handle contract.
    let Some(handle) = (unsafe { handle_mut(reference) }) else {
        return ptr::null_mut();
    };
    match property {
        VPI_NAME => handle.name.as_ptr().cast_mut(),
        VPI_FULL_NAME => handle.full_name.as_ptr().cast_mut(),
        VPI_DEF_NAME => {
            let HandleKind::Object(ObjectRef::Scope(id)) = handle.kind else {
                return ptr::null_mut();
            };
            let Some(name) = with_runtime(|runtime| {
                runtime
                    .reflection()
                    .scope(id)
                    .map(|scope| scope.module_name.clone())
            })
            .flatten() else {
                return ptr::null_mut();
            };
            handle.value_string = Some(c_string(&name));
            handle.value_string.as_ref().unwrap().as_ptr().cast_mut()
        }
        VPI_TYPE => match handle.kind {
            HandleKind::Object(object) => match object_type(object) {
                VPI_MODULE => c"vpiModule".as_ptr().cast_mut(),
                VPI_REG => c"vpiReg".as_ptr().cast_mut(),
                _ => ptr::null_mut(),
            },
            HandleKind::Constant(_) => c"vpiConstant".as_ptr().cast_mut(),
            HandleKind::Callback(_) => c"vpiCallback".as_ptr().cast_mut(),
            HandleKind::Iterator { .. } => c"vpiIterator".as_ptr().cast_mut(),
        },
        _ => ptr::null_mut(),
    }
}

fn value_bits(id: ReflectionSignalId) -> Option<(BigUint, BigUint, usize)> {
    let (identity, width) = with_runtime(|runtime| {
        Some((
            runtime.signal_identity(id)?,
            runtime.reflection().signal(id)?.signal.width,
        ))
    })
    .flatten()?;
    if let Some(forced) = FORCED_VALUES.with_borrow(|forced| forced.get(&identity).cloned()) {
        return Some((forced.value, forced.mask, width));
    }
    with_runtime(|runtime| {
        let signal = runtime.reflection().signal(id)?;
        let (value, mask) = runtime.backend().get_four_state(signal.signal);
        Some((value, mask, signal.signal.width))
    })
    .flatten()
}

fn format_binary(value: &BigUint, mask: &BigUint, width: usize) -> String {
    (0..width)
        .rev()
        .map(|bit| {
            if mask.bit(bit as u64) {
                if value.bit(bit as u64) { 'x' } else { 'z' }
            } else if value.bit(bit as u64) {
                '1'
            } else {
                '0'
            }
        })
        .collect()
}

fn format_hex(value: &BigUint, mask: &BigUint, width: usize) -> String {
    (0..width.div_ceil(4))
        .rev()
        .map(|nibble| {
            let bit = nibble * 4;
            if (0..4).any(|offset| bit + offset < width && mask.bit((bit + offset) as u64)) {
                let all_z = (0..4).all(|offset| {
                    bit + offset >= width
                        || mask.bit((bit + offset) as u64) && !value.bit((bit + offset) as u64)
                });
                if all_z { 'z' } else { 'x' }
            } else {
                let digit = (0..4).fold(0u8, |digit, offset| {
                    digit | (u8::from(value.bit((bit + offset) as u64)) << offset)
                });
                char::from_digit(u32::from(digit), 16).unwrap()
            }
        })
        .collect()
}

fn encode_signal_value(
    handle: &mut VpiHandleObject,
    value: &mut VpiValue,
    bits: &BigUint,
    mask: &BigUint,
    width: usize,
    signed: bool,
) {
    match value.format {
        VPI_INT_VAL => {
            let mut integer = bits.to_u32_digits().first().copied().unwrap_or(0);
            if signed && width > 0 && width < 32 && bits.bit((width - 1) as u64) {
                integer |= u32::MAX << width;
            }
            value.value.integer = integer as i32;
        }
        VPI_SCALAR_VAL => {
            value.value.scalar = if mask.bit(0) {
                if bits.bit(0) { VPI_X } else { VPI_Z }
            } else if bits.bit(0) {
                VPI_1
            } else {
                VPI_0
            };
        }
        VPI_BIN_STR_VAL => {
            handle.value_string = Some(c_string(&format_binary(bits, mask, width)));
            value.value.str_ = handle.value_string.as_ref().unwrap().as_ptr().cast_mut();
        }
        VPI_HEX_STR_VAL => {
            let string = format_hex(bits, mask, width);
            handle.value_string = Some(c_string(&string));
            value.value.str_ = handle.value_string.as_ref().unwrap().as_ptr().cast_mut();
        }
        VPI_VECTOR_VAL => {
            let words = width.div_ceil(32);
            let value_words = bits.to_u32_digits();
            let mask_words = mask.to_u32_digits();
            handle.value_vector.clear();
            handle
                .value_vector
                .extend((0..words).map(|index| VpiVecVal {
                    aval: value_words.get(index).copied().unwrap_or(0) as i32,
                    bval: mask_words.get(index).copied().unwrap_or(0) as i32,
                }));
            value.value.vector = handle.value_vector.as_mut_ptr();
        }
        VPI_SUPPRESS_VAL => {}
        _ => {}
    }
}

unsafe fn write_snapshot_value(
    reference: VpiHandle,
    value: *mut VpiValue,
    bits: &BigUint,
    mask: &BigUint,
) {
    if value.is_null() {
        return;
    }
    // Safety: the caller upholds the VPI handle contract.
    let Some(handle) = (unsafe { handle_mut(reference) }) else {
        return;
    };
    let HandleKind::Object(ObjectRef::Signal(id)) = handle.kind else {
        return;
    };
    let metadata = with_runtime(|runtime| {
        runtime
            .reflection()
            .signal(id)
            .map(|signal| (signal.signal.width, signal.signed))
    })
    .flatten();
    let Some((width, signed)) = metadata else {
        return;
    };
    // Safety: checked non-null above; the caller owns writable value storage.
    encode_signal_value(handle, unsafe { &mut *value }, bits, mask, width, signed);
}

#[unsafe(no_mangle)]
/// Read a signal value in the requested VPI format.
///
/// # Safety
///
/// `reference` must be a live signal handle and `value` must point to writable
/// `VpiValue` storage. Returned string/vector pointers are owned by the handle.
pub unsafe extern "C" fn vpi_get_value(reference: VpiHandle, value: *mut VpiValue) {
    if value.is_null() {
        return;
    }
    // Safety: both pointers follow the VPI ABI contract.
    let Some(handle) = (unsafe { handle_mut(reference) }) else {
        return;
    };
    if let HandleKind::Constant(integer) = handle.kind {
        // Safety: checked non-null above; caller owns the `VpiValue` allocation.
        let value = unsafe { &mut *value };
        if value.format == VPI_INT_VAL {
            value.value.integer = integer;
        }
        return;
    }
    let HandleKind::Object(ObjectRef::Signal(id)) = handle.kind else {
        return;
    };
    let Some((bits, mask, width)) = value_bits(id) else {
        return;
    };
    let signed =
        with_runtime(|runtime| runtime.reflection().signal(id).map(|signal| signal.signed))
            .flatten()
            .unwrap_or(false);
    // Safety: checked non-null above; caller owns the `VpiValue` allocation.
    encode_signal_value(handle, unsafe { &mut *value }, &bits, &mask, width, signed);
}

unsafe fn decode_value(value: *const VpiValue, width: usize) -> Option<(BigUint, BigUint)> {
    if value.is_null() {
        return None;
    }
    // Safety: checked non-null above; caller owns the `VpiValue` allocation.
    let value = unsafe { &*value };
    match value.format {
        VPI_INT_VAL => {
            // Safety: reading the active union member selected by `format`.
            let integer = unsafe { value.value.integer };
            let mut bits = BigUint::from(integer as u32);
            if integer < 0 && width > 32 {
                bits |= ((BigUint::from(1u8) << (width - 32)) - BigUint::from(1u8)) << 32;
            }
            Some((bits, 0u8.into()))
        }
        VPI_SCALAR_VAL => {
            // Safety: reading the active union member selected by `format`.
            match unsafe { value.value.scalar } {
                VPI_0 => Some((0u8.into(), 0u8.into())),
                VPI_1 => Some((1u8.into(), 0u8.into())),
                VPI_X => Some((1u8.into(), 1u8.into())),
                VPI_Z => Some((0u8.into(), 1u8.into())),
                _ => None,
            }
        }
        VPI_VECTOR_VAL => {
            // Safety: reading the active union member selected by `format`.
            let vector = unsafe { value.value.vector };
            if vector.is_null() {
                return None;
            }
            let words = width.div_ceil(32);
            // Safety: the VPI caller supplies at least ceil(width / 32) words.
            let vector = unsafe { std::slice::from_raw_parts(vector, words) };
            let mut bits = BigUint::from(0u8);
            let mut mask = BigUint::from(0u8);
            for (index, word) in vector.iter().enumerate().rev() {
                bits <<= 32usize;
                bits |= BigUint::from(word.aval as u32);
                mask <<= 32usize;
                mask |= BigUint::from(word.bval as u32);
                debug_assert!(index < words);
            }
            Some((bits, mask))
        }
        VPI_BIN_STR_VAL | VPI_HEX_STR_VAL => {
            // Safety: reading the active union member selected by `format`.
            let string = unsafe { value.value.str_ };
            if string.is_null() {
                return None;
            }
            // Safety: the VPI caller promises a valid NUL-terminated string.
            let string = unsafe { CStr::from_ptr(string) }.to_str().ok()?;
            let radix_bits = if value.format == VPI_BIN_STR_VAL {
                1
            } else {
                4
            };
            let mut bits = BigUint::from(0u8);
            let mut mask = BigUint::from(0u8);
            for digit in string.bytes() {
                if digit == b'_' {
                    continue;
                }
                bits <<= radix_bits;
                mask <<= radix_bits;
                match digit {
                    b'0'..=b'9' if digit - b'0' < (1 << radix_bits) => {
                        bits |= BigUint::from(digit - b'0');
                    }
                    b'a'..=b'f' if radix_bits == 4 => {
                        bits |= BigUint::from(digit - b'a' + 10);
                    }
                    b'A'..=b'F' if radix_bits == 4 => {
                        bits |= BigUint::from(digit - b'A' + 10);
                    }
                    b'x' | b'X' => {
                        let unknown = (1u8 << radix_bits) - 1;
                        bits |= BigUint::from(unknown);
                        mask |= BigUint::from(unknown);
                    }
                    b'z' | b'Z' => {
                        let unknown = (1u8 << radix_bits) - 1;
                        mask |= BigUint::from(unknown);
                    }
                    _ => return None,
                }
            }
            if width == 0 {
                return Some((0u8.into(), 0u8.into()));
            }
            let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            Some((bits & &width_mask, mask & width_mask))
        }
        _ => None,
    }
}

#[unsafe(no_mangle)]
/// Apply an immediate signal value.
///
/// # Safety
///
/// `reference` must be a live signal handle. `value` and any format-selected
/// pointer it contains must reference enough readable storage for the signal.
pub unsafe extern "C" fn vpi_put_value(
    reference: VpiHandle,
    value: *const VpiValue,
    when: *const c_void,
    flags: i32,
) -> VpiHandle {
    let inertial_is_immediate = flags == VPI_INERTIAL_DELAY
        && (when.is_null() || {
            // Safety: a non-null VPI delay pointer addresses `s_vpi_time`.
            let delay = unsafe { &*when.cast::<VpiTime>() };
            delay.type_ == 2 && delay.high == 0 && delay.low == 0
        });
    if !matches!(
        flags,
        VPI_NO_DELAY | VPI_INERTIAL_DELAY | VPI_FORCE_FLAG | VPI_RELEASE_FLAG
    ) || (flags == VPI_INERTIAL_DELAY && !inertial_is_immediate)
    {
        return ptr::null_mut();
    }
    // Safety: reference follows the VPI handle contract.
    let Some(ObjectRef::Signal(id)) = (unsafe { object_ref(reference) }) else {
        return ptr::null_mut();
    };
    let identity = with_runtime(|runtime| runtime.signal_identity(id)).flatten();
    let Some(identity) = identity else {
        return ptr::null_mut();
    };
    if flags == VPI_RELEASE_FLAG {
        let released = FORCED_VALUES.with_borrow_mut(|forced| forced.remove(&identity));
        let succeeded = released.is_none_or(|released| {
            with_runtime_mut(|runtime| {
                let Some(signal) = runtime.reflection().signal(id).cloned() else {
                    return false;
                };
                let (released_value, released_mask) = if matches!(
                    signal.direction,
                    SignalDirection::Output | SignalDirection::Internal
                ) {
                    // Releasing a VPI variable retains its forced value until
                    // its next procedural assignment. Continuously driven
                    // outputs are overwritten again by the settle below.
                    (released.value, released.mask)
                } else {
                    (released.deposited_value, released.deposited_mask)
                };
                runtime.release_signal(id);
                let (old_value, old_mask) = runtime.backend().get_four_state(signal.signal);
                let old_level = logic_level(&old_value, &old_mask);
                let new_level = logic_level(&released_value, &released_mask);
                PENDING_WRITES.with_borrow_mut(|pending| {
                    record_pending_write(
                        pending,
                        id,
                        identity,
                        released_value.clone(),
                        released_mask.clone(),
                        signal.domain_kind,
                        old_level,
                        new_level,
                    );
                });
                runtime
                    .backend_mut()
                    .set_four_state(signal.signal, released_value, released_mask);
                true
            })
            .unwrap_or(false)
        });
        return if succeeded && (callbacks::is_running() || flush_pending_writes()) {
            reference
        } else {
            ptr::null_mut()
        };
    }

    let signal_info = with_runtime(|runtime| {
        runtime
            .reflection()
            .signal(id)
            .map(|signal| (signal.signal.width, signal.signal.is_4state))
    })
    .flatten();
    let Some((width, is_4state)) = signal_info else {
        return ptr::null_mut();
    };
    // Safety: value follows the VPI value contract.
    let Some((mut bits, mut mask)) = (unsafe { decode_value(value, width) }) else {
        return ptr::null_mut();
    };
    if width == 0 {
        bits = 0u8.into();
        mask = 0u8.into();
    } else {
        let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        bits &= &width_mask;
        mask &= &width_mask;
        if !is_4state {
            let known_bits = &width_mask ^ &mask;
            bits &= known_bits;
            mask = 0u8.into();
        }
    }
    let force_deposited = if flags == VPI_FORCE_FLAG {
        let deposited = FORCED_VALUES
            .with_borrow(|forced| {
                forced.get(&identity).map(|forced| {
                    (
                        forced.deposited_value.clone(),
                        forced.deposited_mask.clone(),
                    )
                })
            })
            .or_else(|| {
                with_runtime(|runtime| {
                    let signal = runtime.reflection().signal(id)?;
                    Some(runtime.backend().get_four_state(signal.signal))
                })
                .flatten()
            });
        let Some((deposited_value, deposited_mask)) = deposited else {
            return ptr::null_mut();
        };
        Some((deposited_value, deposited_mask))
    } else if FORCED_VALUES.with_borrow_mut(|forced| {
        forced.get_mut(&identity).is_some_and(|forced| {
            forced.deposited_value = bits.clone();
            forced.deposited_mask = mask.clone();
            true
        })
    }) {
        return reference;
    } else {
        None
    };
    let succeeded = with_runtime_mut(|runtime| {
        let Some(signal) = runtime.reflection().signal(id).cloned() else {
            return false;
        };
        let (old_value, old_mask) = runtime.backend().get_four_state(signal.signal);
        let old_level = logic_level(&old_value, &old_mask);
        let new_level = logic_level(&bits, &mask);
        if flags == VPI_FORCE_FLAG {
            if !runtime.force_signal(id, bits.clone(), mask.clone()) {
                return false;
            }
        } else {
            runtime
                .backend_mut()
                .set_four_state(signal.signal, bits.clone(), mask.clone());
        }
        PENDING_WRITES.with_borrow_mut(|pending| {
            record_pending_write(
                pending,
                id,
                identity,
                bits.clone(),
                mask.clone(),
                signal.domain_kind,
                old_level,
                new_level,
            );
        });
        true
    })
    .unwrap_or(false);
    if succeeded {
        if let Some((deposited_value, deposited_mask)) = force_deposited {
            FORCED_VALUES.with_borrow_mut(|forced| {
                forced.insert(
                    identity,
                    ForcedValue {
                        value: bits,
                        mask,
                        deposited_value,
                        deposited_mask,
                    },
                );
            });
        }
    }
    let succeeded = succeeded && (callbacks::is_running() || flush_pending_writes());
    if succeeded {
        reference
    } else {
        ptr::null_mut()
    }
}

fn flush_pending_writes() -> bool {
    let pending = PENDING_WRITES.with_borrow_mut(std::mem::take);
    if !pending.settle {
        return true;
    }
    let mut final_deposits = pending.deposits;
    let mut replay_overrides: HashMap<NativeSignalIdentity, PendingDeposit> = HashMap::default();
    for mut batch in pending.edge_batches {
        for (id, value) in &replay_overrides {
            batch.deposits.insert(*id, value.clone());
        }
        let overridden = replay_overrides.keys().copied().collect::<HashSet<_>>();
        let result: Result<(), celox::RuntimeErrorCode> = with_runtime_mut(|runtime| {
            let active_edges = batch
                .active_signals
                .into_iter()
                .filter_map(|identity| {
                    let deposit = batch.deposits.get(&identity)?;
                    let signal = runtime.reflection().signal(deposit.id)?;
                    if overridden.contains(&identity) {
                        let (old_value, old_mask) = runtime.backend().get_four_state(signal.signal);
                        if !is_active_edge(
                            signal.domain_kind,
                            logic_level(&old_value, &old_mask),
                            logic_level(&deposit.value, &deposit.mask),
                        ) {
                            return None;
                        }
                    }
                    Some(signal.state_address)
                })
                .collect::<Vec<_>>();
            apply_pending_deposits(runtime, &batch.deposits);
            runtime.settle_active_edges_with_context(
                &active_edges,
                RuntimeFormatContext {
                    tb_time: Some(celox_vpi_current_time()),
                    scope: None,
                },
            )
        })
        .unwrap_or(Ok(()));
        if let Err(error) = result {
            callbacks::fail(error.to_string());
            return false;
        }
        process_runtime_events();
        if callbacks::has_error() {
            return false;
        }
        if callbacks::is_running() {
            callbacks::dispatch_value_changes();
            if callbacks::finish_requested() {
                return true;
            }
            let mut callback_iterations = 0usize;
            loop {
                let callback_pending = PENDING_WRITES.with_borrow_mut(std::mem::take);
                if !callback_pending.settle {
                    break;
                }
                callback_iterations += 1;
                if callback_iterations > 1_000_000 {
                    callbacks::fail(
                        "VPI callback writes exceeded 1000000 intermediate settlements".to_string(),
                    );
                    return false;
                }
                let overrides = callback_pending.deposits.clone();
                PENDING_WRITES.with_borrow_mut(|pending| *pending = callback_pending);
                if !flush_pending_writes() {
                    return false;
                }
                if callbacks::finish_requested() {
                    return true;
                }
                replay_overrides.extend(overrides.clone());
                final_deposits.extend(overrides);
                callbacks::dispatch_value_changes();
                if callbacks::finish_requested() {
                    return true;
                }
            }
        }
    }
    let result: Result<(), celox::RuntimeErrorCode> = with_runtime_mut(|runtime| {
        apply_pending_deposits(runtime, &final_deposits);
        runtime.settle_active_edges_with_context(
            &[],
            RuntimeFormatContext {
                tb_time: Some(celox_vpi_current_time()),
                scope: None,
            },
        )
    })
    .unwrap_or(Ok(()));
    if let Err(error) = result {
        callbacks::fail(error.to_string());
        return false;
    }
    process_runtime_events();
    !callbacks::has_error()
}

fn process_runtime_events() {
    let time = celox_vpi_current_time();
    let events = with_runtime_mut(|runtime| {
        runtime.drain_runtime_events_with_context(RuntimeFormatContext {
            tb_time: Some(time),
            scope: None,
        })
    })
    .unwrap_or_default();
    for event in events {
        match event {
            RuntimeEvent::Display { message } => {
                let _ = writeln!(std::io::stdout().lock(), "{message}");
            }
            RuntimeEvent::Write { message } => {
                let _ = write!(std::io::stdout().lock(), "{message}");
            }
            RuntimeEvent::AssertContinue { message } => {
                let _ = writeln!(std::io::stderr().lock(), "assertion failed: {message}");
            }
            RuntimeEvent::AssertFatal { message } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "fatal assertion failed: {message}"
                );
                callbacks::fail(message);
            }
            RuntimeEvent::Missed { count } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "celox-vpi: missed {count} runtime events"
                );
                callbacks::fail(format!(
                    "missed {count} runtime events; a fatal assertion may have been overwritten"
                ));
            }
        }
    }
}

#[unsafe(no_mangle)]
/// Release an object handle.
///
/// # Safety
///
/// `reference` must be null or a live handle returned by this library, and it
/// must not be used or freed again after this call.
pub unsafe extern "C" fn vpi_free_object(reference: VpiHandle) -> i32 {
    if reference.is_null() {
        return 0;
    }
    // Safety: reference follows the VPI handle contract.
    if let Some(HandleKind::Callback(id)) = (unsafe { handle_mut(reference) }).map(|h| &h.kind) {
        return callbacks::remove(*id, reference);
    }
    // Safety: the handle came from `Box::into_raw` and ownership returns here.
    drop(unsafe { Box::from_raw(reference) });
    1
}
