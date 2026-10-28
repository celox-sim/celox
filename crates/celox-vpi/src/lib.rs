//! Minimal IEEE VPI compatibility layer backed by an attached Celox image.
//!
//! This first layer intentionally implements object discovery, hierarchy
//! iteration, properties, immediate values, and zero-delay inertial deposits.
//! Callback scheduling is
//! kept separate because it must share the runtime's simulation-region loop.

#![cfg_attr(
    not(any(target_arch = "x86_64", target_arch = "aarch64")),
    allow(dead_code)
)]

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("celox-vpi currently supports only x86-64 and AArch64");

use std::{
    cell::{Cell, RefCell},
    ffi::{CStr, CString, c_char, c_void},
    io::Write,
    ptr,
};

use celox::{
    BigUint, DomainKind, NativeProgramInstance, NativeSignalIdentity, ReflectionScopeId,
    ReflectionSignalId, RuntimeEvent, RuntimeFormatContext, Severity, SignalDirection,
};
use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

mod callbacks;
pub mod driver;
mod state;

pub use callbacks::{
    CB_AFTER_DELAY, CB_END_OF_SIMULATION, CB_NEXT_SIM_TIME, CB_READ_ONLY_SYNCH,
    CB_READ_WRITE_SYNCH, CB_START_OF_SIMULATION, CB_VALUE_CHANGE, VPI_SCALED_REAL_TIME,
    VPI_SIM_TIME, VPI_SUPPRESS_TIME, VpiCbData, VpiErrorInfo, VpiTime, VpiVlogInfo,
    celox_vpi_current_time, vpi_chk_error, vpi_control, vpi_get_time, vpi_get_vlog_info,
    vpi_handle_by_index, vpi_register_cb, vpi_remove_cb,
};
use state::{Runtime, TimedRuntime, VpiState};

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
pub const VPI_PARAMETER: i32 = 41;
pub const VPI_CONST_TYPE: i32 = 40;
pub const VPI_BINARY_CONST: i32 = 3;
pub const VPI_NET: i32 = 36;
pub const VPI_PORT: i32 = 44;
pub const VPI_REG: i32 = 48;
pub const VPI_REG_ARRAY: i32 = 116;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct VpiSignalIdentity {
    storage: NativeSignalIdentity,
    slice: Option<(usize, usize)>,
}

/// A lazy view of an unpacked array. Storage grows with requested handles,
/// rather than with the number of elements in reflected memories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SignalSelection {
    root: ReflectionSignalId,
    first: usize,
    depth: usize,
}

impl SignalSelection {
    fn root(root: ReflectionSignalId) -> Self {
        Self {
            root,
            first: 0,
            depth: 0,
        }
    }

    /// Resolve storage without cloning hierarchy names or dimension metadata.
    fn signal(self, runtime: &dyn VpiState) -> Option<celox::SignalRef> {
        let root = runtime.reflection().signal(self.root)?;
        let mut signal = root.signal;
        if self.depth == 0 {
            return Some(signal);
        }
        let count = root.unpacked_dims.iter().product::<usize>();
        let remaining = root
            .unpacked_dims
            .get(self.depth..)?
            .iter()
            .product::<usize>();
        if self.first.checked_add(remaining)? > count {
            return None;
        }
        if let Some(mut array) = signal.array_layout {
            signal.offset = signal
                .offset
                .checked_add(self.first.checked_mul(array.element_stride)?)?;
            signal.width = remaining.checked_mul(root.signal.width / count)?;
            array.element_count = remaining;
            signal.array_layout = Some(array);
        }
        Some(signal)
    }

    fn domain_kind(self, runtime: &dyn VpiState) -> Option<DomainKind> {
        let root = runtime.reflection().signal(self.root)?;
        Some(if self.depth == 0 {
            root.domain_kind
        } else {
            DomainKind::Other
        })
    }

    fn resolve(self, runtime: &dyn VpiState) -> Option<celox::ReflectionSignal> {
        let mut signal = runtime.reflection().signal(self.root)?.clone();
        signal.signal = self.signal(runtime)?;
        if self.depth == 0 {
            return Some(signal);
        }
        let count = signal.unpacked_dims.iter().product::<usize>();
        let mut stride = count;
        for (&size, &(left, right)) in signal
            .unpacked_dims
            .iter()
            .zip(&signal.unpacked_ranges)
            .take(self.depth)
        {
            stride /= size;
            let position = (self.first / stride) % size;
            let index = if left <= right {
                left + position as i128
            } else {
                left - position as i128
            };
            let suffix = format!("[{index}]");
            signal.name.push_str(&suffix);
            signal.full_name.push_str(&suffix);
        }
        signal.unpacked_dims.drain(..self.depth);
        signal.unpacked_ranges.drain(..self.depth);
        signal.domain_kind = DomainKind::Other;
        Some(signal)
    }

    fn select(self, runtime: &dyn VpiState, index: i32) -> Option<Self> {
        let root = runtime.reflection().signal(self.root)?;
        let &(left, right) = root.unpacked_ranges.get(self.depth)?;
        let index = i128::from(index);
        if index < left.min(right) || index > left.max(right) {
            return None;
        }
        let position = usize::try_from(if left <= right {
            index - left
        } else {
            left - index
        })
        .ok()?;
        let stride = root
            .unpacked_dims
            .get(self.depth + 1..)?
            .iter()
            .product::<usize>();
        Some(Self {
            root: self.root,
            first: self.first.checked_add(position.checked_mul(stride)?)?,
            depth: self.depth + 1,
        })
    }

    // Some arrays use contiguous bits instead of a byte stride (including
    // scalar arrays and arrays whose uses cannot be lowered as strided loads).
    fn slice(self, runtime: &dyn VpiState) -> Option<(usize, usize)> {
        let root = runtime.reflection().signal(self.root)?;
        if self.depth == 0 || root.signal.array_layout.is_some() {
            return None;
        }
        let element_width = root.signal.width / root.unpacked_dims.iter().product::<usize>();
        let remaining = root
            .unpacked_dims
            .get(self.depth..)?
            .iter()
            .product::<usize>();
        Some((self.first * element_width, remaining * element_width))
    }

    fn width(self, runtime: &dyn VpiState) -> Option<usize> {
        if let Some((_, width)) = self.slice(runtime) {
            Some(width)
        } else {
            Some(self.signal(runtime)?.width)
        }
    }

    fn identity(self, runtime: &dyn VpiState) -> Option<VpiSignalIdentity> {
        let storage = if self.depth == 0 || self.slice(runtime).is_some() {
            runtime.signal_identity(self.root)?
        } else {
            NativeSignalIdentity::State(self.signal(runtime)?)
        };
        Some(VpiSignalIdentity {
            storage,
            slice: self.slice(runtime),
        })
    }

    fn read(self, runtime: &dyn VpiState) -> Option<(BigUint, BigUint)> {
        let signal = self.signal(runtime)?;
        let Some((offset, width)) = self.slice(runtime) else {
            return Some(runtime.get_four_state(signal));
        };
        let (window, shift) = bit_window(signal, offset, width);
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let bits = (runtime.get_four_state(window).0 >> shift) & &mask;
        let unknowns = if signal.is_4state && runtime.four_state() {
            let mut mask_window = window;
            mask_window.offset += signal.width.div_ceil(8);
            (runtime.get_four_state(mask_window).0 >> shift) & mask
        } else {
            BigUint::default()
        };
        Some((bits, unknowns))
    }

    fn write(self, runtime: &mut dyn VpiState, bits: BigUint, mask: BigUint) {
        let Some(signal) = self.signal(runtime) else {
            return;
        };
        let Some((offset, width)) = self.slice(runtime) else {
            runtime.set_four_state(signal, bits, mask);
            return;
        };
        let (window, shift) = bit_window(signal, offset, width);
        let selected = ((BigUint::from(1u8) << width) - BigUint::from(1u8)) << shift;
        let whole = (BigUint::from(1u8) << window.width) - BigUint::from(1u8);
        let retained = &whole ^ &selected;
        let old = runtime.get_four_state(window).0;
        runtime.set_four_state(
            window,
            (old & &retained) | ((bits << shift) & &selected),
            BigUint::default(),
        );
        if signal.is_4state && runtime.four_state() {
            let mut mask_window = window;
            mask_window.offset += signal.width.div_ceil(8);
            let old = runtime.get_four_state(mask_window).0;
            runtime.set_four_state(
                mask_window,
                (old & retained) | ((mask << shift) & selected),
                BigUint::default(),
            );
        }
    }
}

/// Small byte-aligned window around a logical bit slice. Preserve the boundary
/// bits on writes; even a huge bit-packed RAM needs only the selected bytes.
fn bit_window(signal: celox::SignalRef, offset: usize, width: usize) -> (celox::SignalRef, usize) {
    let shift = offset % 8;
    (
        celox::SignalRef {
            offset: signal.offset + offset / 8,
            width: (shift + width).div_ceil(8) * 8,
            is_4state: false,
            array_layout: None,
        },
        shift,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectRef {
    Scope(ReflectionScopeId),
    Signal(SignalSelection),
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
    value_string: Vec<u8>,
    value_vector: Vec<VpiVecVal>,
}

pub type VpiHandle = *mut VpiHandleObject;

/// State owned by one VPI simulation session. The C ABI selects one session
/// per process because standard VPI entry points carry no host context.
#[derive(Default)]
struct VpiHost {
    runtime: RefCell<Option<Runtime>>,
    /// Untimed state can only change through the host's mutation helpers.
    /// Timed slot observers keep polling because kernels can update their state
    /// between observer callbacks without returning through those helpers.
    poll_only_when_dirty: bool,
    values_dirty: Cell<bool>,
    forced_values: RefCell<HashMap<VpiSignalIdentity, ForcedValue>>,
    pending_writes: RefCell<PendingWrites>,
    // Zero-delay inertial deposits become visible in the next update round.
    inertial_writes: RefCell<HashMap<VpiSignalIdentity, PendingDeposit>>,
    callbacks: RefCell<callbacks::CallbackState>,
    vlog: RefCell<Option<callbacks::VlogStorage>>,
}

struct HostRegistration {
    owner: std::thread::ThreadId,
    // An owning Rc reference, accessed only after checking the owner thread.
    address: usize,
}

static ACTIVE_HOST: std::sync::Mutex<Option<HostRegistration>> = std::sync::Mutex::new(None);

/// Owns the active process VPI session. Keep this guard alive while calling VPI
/// or plugin code. A session and all its callbacks stay on the installing thread.
#[must_use = "keep the VPI session alive while invoking plugins"]
pub struct VpiSession {
    host: std::rc::Rc<VpiHost>,
    registered: bool,
}

impl VpiSession {
    /// Capture the last evaluated HDL values without dispatching callbacks.
    ///
    /// VPI callbacks, pending writes, forces and plugin memory are not saved.
    /// The returned file is for inspection/diff or HDL-only initialization;
    /// it cannot resume a VPI session. Call between dispatch runs or from a
    /// read-only synchronization callback for settled values. Other callback
    /// phases capture exactly the values currently visible to VPI reads.
    pub fn save_state(&self) -> Result<celox::StateFile, String> {
        capture_host_state(&self.host)
    }

    // The C loader has no Rust caller to retain a guard. Transfer its lifetime
    // to the explicit celox_vpi_clear_runtime/clear_runtime boundary instead.
    fn detach(mut self) {
        self.registered = false;
    }
}

impl Drop for VpiSession {
    fn drop(&mut self) {
        if self.registered {
            unregister_host(std::rc::Rc::as_ptr(&self.host) as usize);
        }
    }
}

fn unregister_host(address: usize) {
    let registration = {
        let mut active = ACTIVE_HOST
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active.as_ref().is_some_and(|host| {
            host.owner == std::thread::current().id() && host.address == address
        }) {
            // Safety: this registration owns a live Rc and the owner-thread
            // check above excludes cross-thread access to its RefCell fields.
            let host = unsafe { &*(address as *const VpiHost) };
            let mut callbacks = host.callbacks.borrow_mut();
            if callbacks.running {
                // Keep this host selected through all remaining callback phases.
                // Reentrant installation must fail until the run unwinds.
                callbacks.finish = true;
                callbacks.detach_on_stop = true;
                None
            } else {
                active.take()
            }
        } else {
            None
        }
    };
    if let Some(registration) = registration {
        // Safety: registration owns one Rc reference created by into_raw. Only
        // the owner thread can remove it. Dispatch clones its own Rc before
        // releasing the registry lock, so reentrant teardown cannot invalidate
        // a host still in use. No registry lock is held while dropping state.
        drop(unsafe { std::rc::Rc::from_raw(registration.address as *const VpiHost) });
    }
}

fn borrow_cell<T, R>(cell: &RefCell<T>, operation: impl FnOnce(&T) -> R) -> R {
    operation(&cell.borrow())
}

fn borrow_cell_mut<T, R>(cell: &RefCell<T>, operation: impl FnOnce(&mut T) -> R) -> R {
    operation(&mut cell.borrow_mut())
}

fn host_is_active() -> bool {
    ACTIVE_HOST
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .is_some_and(|host| host.owner == std::thread::current().id())
}

fn selected_host() -> Option<std::rc::Rc<VpiHost>> {
    let active = ACTIVE_HOST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    active
        .as_ref()
        .filter(|host| host.owner == std::thread::current().id())
        .map(|host| {
            let pointer = host.address as *const VpiHost;
            // Safety: the owning registration stays live under the lock and
            // only its owner thread touches this non-Send Rc. The cloned Rc
            // keeps the host alive across callbacks and reentrant teardown.
            unsafe {
                std::rc::Rc::increment_strong_count(pointer);
                std::rc::Rc::from_raw(pointer)
            }
        })
}

fn with_host<R>(operation: impl FnOnce(&VpiHost) -> R) -> R {
    // An unbound/foreign-thread C call sees an empty host, never another
    // thread's non-Send simulation state.
    let host = selected_host().unwrap_or_default();
    operation(&host)
}

impl Drop for VpiHost {
    fn drop(&mut self) {
        for registration in std::mem::take(self.callbacks.get_mut())
            .callbacks
            .into_values()
        {
            // Safety: the host owns queued callback handles; firing callbacks
            // were removed from the registry before entering plugin code.
            drop(unsafe { Box::from_raw(registration.handle) });
        }
    }
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
    deposits: HashMap<VpiSignalIdentity, PendingDeposit>,
    edge_batches: Vec<PendingEdgeBatch>,
    open_edge_batch: Option<usize>,
    settle: bool,
}

struct PendingEdgeBatch {
    active_signals: Vec<VpiSignalIdentity>,
    transition_signals: Vec<VpiSignalIdentity>,
    deposits: HashMap<VpiSignalIdentity, PendingDeposit>,
}

#[derive(Clone)]
struct PendingDeposit {
    id: SignalSelection,
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

impl LogicLevel {
    /// The edge level of [`celox_design::edges`].
    fn edge_level(self) -> u8 {
        use celox_design::edges::LEVEL_UNKNOWN;
        match self {
            LogicLevel::Zero => 0,
            LogicLevel::One => 1,
            LogicLevel::X => LEVEL_UNKNOWN | 1,
            LogicLevel::Z => LEVEL_UNKNOWN,
        }
    }
}

/// Whether a write from `old` to `new` is an active edge of a domain of
/// `kind` (IEEE 1800-2023 Table 9-2). A domain without an edge has none.
fn is_active_edge(kind: DomainKind, old: LogicLevel, new: LogicLevel) -> bool {
    celox_design::edges::polarity(kind).is_some()
        && celox_design::edges::settled_edge::<true>(Some(kind), old.edge_level(), new.edge_level())
}

fn record_pending_write(
    pending: &mut PendingWrites,
    id: SignalSelection,
    identity: VpiSignalIdentity,
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
    deposits: &HashMap<VpiSignalIdentity, PendingDeposit>,
) {
    for deposit in deposits.values() {
        deposit
            .id
            .write(runtime, deposit.value.clone(), deposit.mask.clone());
    }
}

/// Install an instance explicitly. This is primarily useful to an embedding
/// runtime which has already selected the attached image. Retain the returned
/// guard for the entire plugin lifetime. Overlapping sessions are rejected,
/// including installation from a callback before its current host has stopped.
pub fn install_runtime(instance: NativeProgramInstance) -> Result<VpiSession, String> {
    install(Runtime::Untimed(instance))
}

/// Install a timed simulation of `image`, which runs the design's processes
/// in step with the VPI time callbacks. Retain the returned guard as for
/// [`install_runtime`].
pub fn install_timed_runtime(
    image: celox::NativeProgramImage,
    simulation: celox::Simulation<celox::NativeBackend>,
) -> Result<VpiSession, String> {
    install(Runtime::Timed(TimedRuntime::new(image, simulation)))
}

fn install(runtime: Runtime) -> Result<VpiSession, String> {
    let mut host = VpiHost::default();
    host.poll_only_when_dirty = matches!(runtime, Runtime::Untimed(_));
    host.values_dirty.set(true);
    *host.runtime.get_mut() = Some(runtime);
    let host = std::rc::Rc::new(host);
    let mut active = ACTIVE_HOST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if active.is_some() {
        return Err("a VPI session is already active; VPI supports one host per process".into());
    }
    let pointer = std::rc::Rc::into_raw(host.clone());
    *active = Some(HostRegistration {
        owner: std::thread::current().id(),
        address: pointer as usize,
    });
    Ok(VpiSession {
        host,
        registered: true,
    })
}

fn capture_host_state(host: &VpiHost) -> Result<celox::StateFile, String> {
    let runtime = host
        .runtime
        .try_borrow()
        .map_err(|_| "VPI runtime is executing; capture at a callback boundary")?;
    runtime
        .as_ref()
        .ok_or("VPI runtime is unavailable")?
        .save_state()
        .map_err(|error| error.to_string())
}

/// Write a diagnostic HDL state file for the active VPI session.
///
/// Returns 1 on success, 0 for an unavailable/busy runtime, invalid UTF-8 path,
/// unsupported state value, or I/O failure. Uses the capture contract of
/// [`VpiSession::save_state`]. Existing files at `path` are overwritten.
///
/// # Safety
/// `path` must be null or point to a readable NUL-terminated UTF-8 string
/// for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn celox_vpi_save_state(path: *const c_char) -> i32 {
    if path.is_null() {
        return 0;
    }
    // Safety: the caller supplies a readable C string.
    let Ok(path) = unsafe { CStr::from_ptr(path) }.to_str() else {
        return 0;
    };
    let Ok(state) = with_host(capture_host_state) else {
        return 0;
    };
    let Ok(file) = std::fs::File::create(path) else {
        return 0;
    };
    i32::from(state.write_to(file).is_ok())
}

/// Unregister the current thread's host. During callback dispatch this requests
/// finish and defers unregistration until the callback run returns.
pub fn clear_runtime() {
    let address = {
        let active = ACTIVE_HOST
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        active
            .as_ref()
            .filter(|host| host.owner == std::thread::current().id())
            .map(|host| host.address)
    };
    if let Some(address) = address {
        unregister_host(address);
    }
}

/// Release a session installed by the C executable loader on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_clear_runtime() {
    clear_runtime();
}

/// Run VPI simulation regions until `vpiFinish` or until no future activity
/// remains. Returns true when the test requested a normal finish.
pub fn run_callbacks() -> bool {
    run_callbacks_result().unwrap_or(false)
}

// A Rust-side failure must not leave the process ABI bound to an abandoned
// scheduler. Inner RefCell borrows unwind before this outer guard is dropped.
struct CallbackRunCleanup;

impl Drop for CallbackRunCleanup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            with_host(|host| borrow_cell_mut(&host.callbacks, |state| state.running = false));
            clear_runtime();
        }
    }
}

/// Run callbacks while preserving fatal DUT diagnostics for executable hosts.
pub fn run_callbacks_result() -> Result<bool, String> {
    if callbacks::is_running() {
        return Err("VPI callback dispatch is already running".into());
    }
    let _cleanup = CallbackRunCleanup;
    process_runtime_events();
    if let Some(error) = callbacks::take_error() {
        return Err(error);
    }
    let timed = with_timed_mut(|_| ()).is_some();
    let finished = if timed {
        callbacks::run_timed()
    } else {
        callbacks::run()
    };
    let error = callbacks::take_error();
    if with_host(|host| borrow_cell(&host.callbacks, |state| state.detach_on_stop)) {
        clear_runtime();
    }
    match error {
        Some(error) => Err(error),
        None => Ok(finished),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_load_current_executable() -> i32 {
    // Safety: the VPI host only loads the image attached to its own trusted
    // executable artifact.
    match unsafe { NativeProgramInstance::from_current_executable() } {
        Ok(instance) => match install_runtime(instance) {
            Ok(session) => {
                session.detach();
                1
            }
            Err(_) => 0,
        },
        Err(_) => 0,
    }
}

fn with_runtime<R>(operation: impl FnOnce(&dyn VpiState) -> R) -> Option<R> {
    with_host(|host| {
        borrow_cell(&host.runtime, |runtime| {
            runtime.as_ref().map(|runtime| operation(runtime.state()))
        })
    })
}

fn with_runtime_mut<R>(
    host: &VpiHost,
    operation: impl FnOnce(&mut dyn VpiState) -> R,
) -> Option<R> {
    borrow_cell_mut(&host.runtime, |runtime| {
        let result = runtime
            .as_mut()
            .map(|runtime| operation(runtime.state_mut()));
        if result.is_some() {
            host.values_dirty.set(true);
        }
        result
    })
}

/// The untimed program instance of the session, whose regions the VPI host
/// schedules itself; `None` for a timed simulation.
fn with_instance_mut<R>(
    host: &VpiHost,
    operation: impl FnOnce(&mut NativeProgramInstance) -> R,
) -> Option<R> {
    borrow_cell_mut(&host.runtime, |runtime| match runtime {
        Some(Runtime::Untimed(instance)) => {
            let result = operation(instance);
            host.values_dirty.set(true);
            Some(result)
        }
        _ => None,
    })
}

/// The timed runtime of the session, if it has one.
fn with_timed_mut<R>(operation: impl FnOnce(&mut TimedRuntime) -> R) -> Option<R> {
    with_host(|host| {
        borrow_cell_mut(&host.runtime, |runtime| match runtime {
            Some(Runtime::Timed(timed)) => Some(operation(timed)),
            _ => None,
        })
    })
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
        ObjectRef::Signal(id) => id
            .resolve(runtime)
            .map(|signal| (signal.name.clone(), signal.full_name.clone())),
    })
    .flatten()
    .unwrap_or_default();
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Object(object),
        name: c_string(&name),
        full_name: c_string(&full_name),
        value_string: Vec::new(),
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
        value_string: Vec::new(),
        value_vector: Vec::new(),
    }))
}

fn new_constant_handle(value: i32) -> VpiHandle {
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Constant(value),
        name: c_string(""),
        full_name: c_string(""),
        value_string: Vec::new(),
        value_vector: Vec::new(),
    }))
}

fn new_callback_handle(id: u64) -> VpiHandle {
    Box::into_raw(Box::new(VpiHandleObject {
        kind: HandleKind::Callback(id),
        name: c_string(""),
        full_name: c_string(""),
        value_string: Vec::new(),
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
        let reflection = runtime.reflection();
        if let Some((id, _)) = reflection.scope_by_name(name) {
            return Some(ObjectRef::Scope(id));
        }
        if let Some((id, _)) = reflection.signal_by_name(name) {
            return Some(ObjectRef::Signal(SignalSelection::root(id)));
        }
        let mut prefix = name;
        let mut indices = Vec::new();
        while let Some(end) = prefix.strip_suffix(']') {
            let (base, index) = end.rsplit_once('[')?;
            indices.push(index.parse::<i32>().ok()?);
            prefix = base;
            if let Some((id, _)) = reflection.signal_by_name(prefix) {
                let mut selection = SignalSelection::root(id);
                for index in indices.iter().rev() {
                    selection = selection.select(runtime, *index)?;
                }
                return Some(ObjectRef::Signal(selection));
            }
        }
        None
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
        let bounds = with_runtime(|runtime| {
            let signal = id.resolve(runtime)?;
            Some(
                signal
                    .unpacked_ranges
                    .first()
                    .copied()
                    .unwrap_or((id.width(runtime)?.saturating_sub(1) as i128, 0)),
            )
        })
        .flatten();
        return bounds
            .and_then(|(left, right)| {
                i32::try_from(if kind == VPI_LEFT_RANGE { left } else { right }).ok()
            })
            .map_or(ptr::null_mut(), new_constant_handle);
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
        ObjectRef::Signal(id) => {
            if kind == VPI_PARENT {
                // IEEE 1800-2023 37.17: largest containing unpacked array.
                (id.depth != 0).then(|| ObjectRef::Signal(SignalSelection::root(id.root)))
            } else {
                id.resolve(runtime)
                    .map(|signal| ObjectRef::Scope(signal.parent))
            }
        }
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
            (
                VPI_PORT | VPI_REG | VPI_REG_ARRAY | VPI_NET | VPI_VARIABLES | VPI_PARAMETER,
                Some(ObjectRef::Scope(id)),
            ) => reflection
                .scope(id)
                .map(|scope| {
                    scope
                        .signals
                        .iter()
                        .copied()
                        .filter(|signal_id| {
                            let signal = reflection.signal(*signal_id).unwrap();
                            if signal.is_parameter {
                                return kind == VPI_PARAMETER;
                            }
                            match kind {
                                VPI_PORT => signal.direction != SignalDirection::Internal,
                                VPI_REG => signal.unpacked_dims.is_empty(),
                                VPI_REG_ARRAY => !signal.unpacked_dims.is_empty(),
                                VPI_VARIABLES => signal.direction == SignalDirection::Internal,
                                VPI_NET => false,
                                _ => false,
                            }
                        })
                        .map(|id| ObjectRef::Signal(SignalSelection::root(id)))
                        .collect()
                })
                .unwrap_or_default(),
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
        ObjectRef::Signal(id) => with_runtime(|runtime| {
            let Some(signal) = id.resolve(runtime) else {
                return 0;
            };
            if !signal.unpacked_dims.is_empty() {
                VPI_REG_ARRAY
            } else if signal.is_parameter {
                VPI_PARAMETER
            } else {
                VPI_REG
            }
        })
        .unwrap_or(0),
    }
}

#[unsafe(no_mangle)]
/// Read an integer property from an object.
///
/// # Safety
///
/// `reference` must be a live handle returned by this library.
pub unsafe extern "C" fn vpi_get(property: i32, reference: VpiHandle) -> i32 {
    if reference.is_null() && matches!(property, VPI_TIME_UNIT | VPI_TIME_PRECISION) {
        return with_runtime(|runtime| runtime.time_precision().exponent())
            .unwrap_or_else(|| celox_design::TimePrecision::default().exponent());
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
        VPI_TIME_UNIT | VPI_TIME_PRECISION => with_runtime(|runtime| {
            let reflection = runtime.reflection();
            let scope = match object {
                ObjectRef::Scope(id) => reflection.scope(id),
                ObjectRef::Signal(id) => reflection
                    .signal(id.root)
                    .and_then(|signal| reflection.scope(signal.parent)),
            };
            scope.map_or(0, |scope| {
                if property == VPI_TIME_UNIT {
                    scope.time_scale.unit.exponent()
                } else {
                    scope.time_scale.precision.exponent()
                }
            })
        })
        .unwrap_or(0),
        VPI_TYPE => object_type(object),
        VPI_CONST_TYPE => {
            if object_type(object) == VPI_PARAMETER {
                VPI_BINARY_CONST
            } else {
                0
            }
        }
        VPI_TOP_MODULE => i32::from(object == ObjectRef::Scope(ReflectionScopeId(0))),
        VPI_SIZE | VPI_SCALAR | VPI_VECTOR | VPI_DIRECTION | VPI_SIGNED => {
            with_runtime(|runtime| {
                let ObjectRef::Signal(id) = object else {
                    return 0;
                };
                let Some(signal) = id.resolve(runtime) else {
                    return 0;
                };
                match property {
                    VPI_SIZE => i32::try_from(
                        signal
                            .unpacked_dims
                            .first()
                            .copied()
                            .unwrap_or(id.width(runtime).unwrap_or(0)),
                    )
                    .unwrap_or(i32::MAX),
                    VPI_SCALAR => i32::from(
                        id.width(runtime).unwrap_or(0)
                            / signal.unpacked_dims.iter().product::<usize>()
                            == 1,
                    ),
                    VPI_VECTOR => i32::from(
                        id.width(runtime).unwrap_or(0)
                            / signal.unpacked_dims.iter().product::<usize>()
                            != 1,
                    ),
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
            handle.value_string.clear();
            handle
                .value_string
                .extend(name.bytes().filter(|byte| *byte != 0));
            handle.value_string.push(0);
            handle.value_string.as_mut_ptr().cast()
        }
        VPI_TYPE => match handle.kind {
            HandleKind::Object(object) => match object_type(object) {
                VPI_MODULE => c"vpiModule".as_ptr().cast_mut(),
                VPI_REG => c"vpiReg".as_ptr().cast_mut(),
                VPI_REG_ARRAY => c"vpiRegArray".as_ptr().cast_mut(),
                _ => ptr::null_mut(),
            },
            HandleKind::Constant(_) => c"vpiConstant".as_ptr().cast_mut(),
            HandleKind::Callback(_) => c"vpiCallback".as_ptr().cast_mut(),
            HandleKind::Iterator { .. } => c"vpiIterator".as_ptr().cast_mut(),
        },
        _ => ptr::null_mut(),
    }
}

fn value_bits_at(host: &VpiHost, id: SignalSelection) -> Option<(BigUint, BigUint, usize)> {
    borrow_cell(&host.runtime, |runtime| {
        let runtime = runtime.as_ref()?.state();
        let signal = runtime.reflection().signal(id.root)?;
        if id.depth != signal.unpacked_dims.len() {
            return None;
        }
        let width = id.width(runtime)?;
        {
            let forced_values = host.forced_values.borrow();
            if !forced_values.is_empty()
                && let Some(forced) = forced_values.get(&id.identity(runtime)?)
            {
                return Some((forced.value.clone(), forced.mask.clone(), width));
            }
        }
        let (value, mask) = id.read(runtime)?;
        Some((value, mask, width))
    })
}

fn format_binary(buffer: &mut Vec<u8>, value: &BigUint, mask: &BigUint, width: usize) {
    buffer.resize(width, 0);
    let mut values = value.iter_u64_digits();
    let mut masks = mask.iter_u64_digits();
    for chunk in buffer.rchunks_mut(64) {
        let value = values.next().unwrap_or(0);
        let mask = masks.next().unwrap_or(0);
        for (bit, digit) in chunk.iter_mut().rev().enumerate() {
            let index = (((mask >> bit) & 1) << 1) | ((value >> bit) & 1);
            *digit = b"01zx"[index as usize];
        }
    }
    buffer.push(0);
}

fn format_hex(buffer: &mut Vec<u8>, value: &BigUint, mask: &BigUint, width: usize) {
    buffer.resize(width.div_ceil(4), 0);
    let mut values = value.iter_u64_digits();
    let mut masks = mask.iter_u64_digits();
    for (word, chunk) in buffer.rchunks_mut(16).enumerate() {
        let value = values.next().unwrap_or(0);
        let mask = masks.next().unwrap_or(0);
        for (nibble, digit) in chunk.iter_mut().rev().enumerate() {
            let shift = nibble * 4;
            let valid = (1u8 << (width - word * 64 - shift).min(4)) - 1;
            let bits = (value >> shift) as u8 & 15;
            let unknown = (mask >> shift) as u8 & valid;
            *digit = if unknown == 0 {
                b"0123456789abcdef"[usize::from(bits)]
            } else if unknown == valid && bits & valid == 0 {
                b'z'
            } else {
                b'x'
            };
        }
    }
    buffer.push(0);
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
            let mut integer = bits.iter_u32_digits().next().unwrap_or(0);
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
            format_binary(&mut handle.value_string, bits, mask, width);
            value.value.str_ = handle.value_string.as_mut_ptr().cast();
        }
        VPI_HEX_STR_VAL => {
            format_hex(&mut handle.value_string, bits, mask, width);
            value.value.str_ = handle.value_string.as_mut_ptr().cast();
        }
        VPI_VECTOR_VAL => {
            let words = width.div_ceil(32);
            let mut value_words = bits.iter_u32_digits();
            let mut mask_words = mask.iter_u32_digits();
            handle.value_vector.clear();
            handle.value_vector.extend((0..words).map(|_| VpiVecVal {
                aval: value_words.next().unwrap_or(0) as i32,
                bval: mask_words.next().unwrap_or(0) as i32,
            }));
            value.value.vector = handle.value_vector.as_mut_ptr();
        }
        VPI_SUPPRESS_VAL => {}
        _ => {}
    }
}

unsafe fn write_snapshot_value(
    host: &VpiHost,
    reference: VpiHandle,
    value: *mut VpiValue,
    bits: &BigUint,
    mask: &BigUint,
) {
    if value.is_null() || reference.is_null() {
        return;
    }
    // Safety: the caller has selected this host and keeps the callback's
    // object handle live through dispatch.
    let handle = unsafe { &mut *reference };
    let HandleKind::Object(ObjectRef::Signal(id)) = handle.kind else {
        return;
    };
    let metadata = borrow_cell(&host.runtime, |runtime| {
        let runtime = runtime.as_ref()?.state();
        runtime
            .reflection()
            .signal(id.root)
            .and_then(|signal| Some((id.width(runtime)?, signal.signed)))
    });
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
    if value.is_null() || reference.is_null() {
        return;
    }
    // The selected host belongs to this thread. Resolve it once for both
    // handle access and signal metadata instead of reentering the registry.
    let Some(host) = selected_host() else {
        return;
    };
    // Safety: the caller supplies a live handle; selection above rejects
    // foreign-thread/unbound calls before dereferencing it.
    let handle = unsafe { &mut *reference };
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
    let Some((bits, mask, width)) = value_bits_at(&host, id) else {
        return;
    };
    let signed = borrow_cell(&host.runtime, |runtime| {
        runtime
            .as_ref()
            .and_then(|runtime| runtime.state().reflection().signal(id.root))
            .is_some_and(|signal| signal.signed)
    });
    // Safety: checked non-null above; caller owns the `VpiValue` allocation.
    encode_signal_value(handle, unsafe { &mut *value }, &bits, &mask, width, signed);
}

fn decode_digit(digit: u8, radix_bits: u32) -> Option<(u8, u8)> {
    match digit {
        b'0'..=b'9' if digit - b'0' < (1 << radix_bits) => Some((digit - b'0', 0)),
        b'a'..=b'f' if radix_bits == 4 => Some((digit - b'a' + 10, 0)),
        b'A'..=b'F' if radix_bits == 4 => Some((digit - b'A' + 10, 0)),
        b'x' | b'X' => {
            let unknown = (1u8 << radix_bits) - 1;
            Some((unknown, unknown))
        }
        b'z' | b'Z' => Some((0, (1u8 << radix_bits) - 1)),
        _ => None,
    }
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
                if width <= 64 {
                    bits = BigUint::from(integer as i64 as u64);
                } else {
                    bits |= ((BigUint::from(1u8) << (width - 32)) - BigUint::from(1u8)) << 32;
                }
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
            if width <= 64 {
                let mut bits = 0u64;
                let mut mask = 0u64;
                for word in vector.iter().rev() {
                    bits = (bits << 32) | u64::from(word.aval as u32);
                    mask = (mask << 32) | u64::from(word.bval as u32);
                }
                return Some((BigUint::from(bits), BigUint::from(mask)));
            }
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
            if width <= 64 {
                let mut bits = 0u64;
                let mut mask = 0u64;
                for digit in string.bytes() {
                    if digit == b'_' {
                        continue;
                    }
                    // Truncation to the destination width is performed by
                    // the caller. Still validate every digit, including an
                    // overlong string's discarded prefix.
                    bits <<= radix_bits;
                    mask <<= radix_bits;
                    let (value_digit, mask_digit) = decode_digit(digit, radix_bits)?;
                    bits |= u64::from(value_digit);
                    mask |= u64::from(mask_digit);
                }
                return Some((BigUint::from(bits), BigUint::from(mask)));
            }
            let mut bits = BigUint::from(0u8);
            let mut mask = BigUint::from(0u8);
            for digit in string.bytes() {
                if digit == b'_' {
                    continue;
                }
                let (value_digit, mask_digit) = decode_digit(digit, radix_bits)?;
                bits <<= radix_bits;
                mask <<= radix_bits;
                bits |= BigUint::from(value_digit);
                mask |= BigUint::from(mask_digit);
            }
            Some((bits, mask))
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
    if reference.is_null() {
        return ptr::null_mut();
    }
    let Some(host) = selected_host() else {
        return ptr::null_mut();
    };
    // A ReadOnly callback may not write (IEEE 1800-2023 38.36.2, Table 38-6).
    if borrow_cell(&host.callbacks, |state| state.read_only) {
        return ptr::null_mut();
    }
    let inertial_is_immediate = flags == VPI_INERTIAL_DELAY
        && (when.is_null() || {
            // Safety: a non-null VPI delay pointer addresses `s_vpi_time`.
            let delay = unsafe { &*when.cast::<VpiTime>() };
            match delay.type_ {
                VPI_SIM_TIME => delay.high == 0 && delay.low == 0,
                VPI_SCALED_REAL_TIME => delay.real == 0.0,
                _ => false,
            }
        });
    if !matches!(
        flags,
        VPI_NO_DELAY | VPI_INERTIAL_DELAY | VPI_FORCE_FLAG | VPI_RELEASE_FLAG
    ) || (flags == VPI_INERTIAL_DELAY && !inertial_is_immediate)
    {
        return ptr::null_mut();
    }
    // Safety: the owner-thread check above selected and retained this host
    // before accessing the caller's live, non-null handle.
    let HandleKind::Object(ObjectRef::Signal(id)) = (unsafe { &*reference }).kind else {
        return ptr::null_mut();
    };
    if id.depth != 0 && matches!(flags, VPI_FORCE_FLAG | VPI_RELEASE_FLAG) {
        return ptr::null_mut();
    }
    let signal_info = borrow_cell(&host.runtime, |runtime| {
        let runtime = runtime.as_ref()?.state();
        let root = runtime.reflection().signal(id.root)?;
        if root.is_parameter || id.depth != root.unpacked_dims.len() {
            return None;
        }
        Some((
            id.identity(runtime)?,
            id.width(runtime)?,
            id.signal(runtime)?.is_4state && runtime.four_state(),
        ))
    });
    let Some((identity, width, is_4state)) = signal_info else {
        return ptr::null_mut();
    };
    if flags == VPI_RELEASE_FLAG {
        let released = borrow_cell_mut(&host.forced_values, |forced| forced.remove(&identity));
        let succeeded = released.is_none_or(|released| {
            with_runtime_mut(&host, |runtime| {
                let Some(signal) = id.resolve(runtime) else {
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
                runtime.release(id.root);
                if runtime.schedules_writes() {
                    let (old_value, old_mask) = runtime.get_four_state(signal.signal);
                    let old_level = logic_level(&old_value, &old_mask);
                    let new_level = logic_level(&released_value, &released_mask);
                    borrow_cell_mut(&host.pending_writes, |pending| {
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
                }
                runtime.set_four_state(signal.signal, released_value, released_mask);
                true
            })
            .unwrap_or(false)
        });
        return if succeeded
            && (callbacks::is_running_for_host(&host) || flush_pending_writes_for_host(&host))
        {
            reference
        } else {
            ptr::null_mut()
        };
    }

    // Safety: value follows the VPI value contract.
    let Some((mut bits, mut mask)) = (unsafe { decode_value(value, width) }) else {
        return ptr::null_mut();
    };
    if width == 0 {
        bits = 0u8.into();
        mask = 0u8.into();
    } else {
        if bits.bits() > width as u64 || mask.bits() > width as u64 {
            let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            bits &= &width_mask;
            mask &= &width_mask;
        }
        if !is_4state && mask.bits() != 0 {
            let unknown_bits = &bits & &mask;
            bits ^= unknown_bits;
            mask = 0u8.into();
        }
    }
    if flags == VPI_INERTIAL_DELAY {
        host.inertial_writes.borrow_mut().insert(
            identity,
            PendingDeposit {
                id,
                value: bits,
                mask,
            },
        );
        return reference;
    }
    if apply_signal_value(&host, id, identity, bits, mask, flags) {
        reference
    } else {
        ptr::null_mut()
    }
}

fn apply_signal_value(
    host: &VpiHost,
    id: SignalSelection,
    identity: VpiSignalIdentity,
    bits: BigUint,
    mask: BigUint,
    flags: i32,
) -> bool {
    let force_deposited = if flags == VPI_FORCE_FLAG {
        let deposited = borrow_cell(&host.forced_values, |forced| {
            forced.get(&identity).map(|forced| {
                (
                    forced.deposited_value.clone(),
                    forced.deposited_mask.clone(),
                )
            })
        })
        .or_else(|| {
            borrow_cell(&host.runtime, |runtime| {
                let runtime = runtime.as_ref()?.state();
                Some(runtime.get_four_state(id.signal(runtime)?))
            })
        });
        let Some((deposited_value, deposited_mask)) = deposited else {
            return false;
        };
        Some((deposited_value, deposited_mask))
    } else if borrow_cell_mut(&host.forced_values, |forced| {
        forced.get_mut(&identity).is_some_and(|forced| {
            forced.deposited_value = bits.clone();
            forced.deposited_mask = mask.clone();
            true
        })
    }) {
        return true;
    } else {
        None
    };
    let succeeded = with_runtime_mut(host, |runtime| {
        let Some(domain_kind) = id.domain_kind(runtime) else {
            return false;
        };
        let Some((old_value, old_mask)) = id.read(runtime) else {
            return false;
        };
        let old_level = logic_level(&old_value, &old_mask);
        let new_level = logic_level(&bits, &mask);
        if flags == VPI_FORCE_FLAG {
            if !runtime.force(id.root, bits.clone(), mask.clone()) {
                return false;
            }
        } else {
            id.write(runtime, bits.clone(), mask.clone());
        }
        // A timed simulation schedules the edges of its host writes itself.
        if runtime.schedules_writes() {
            borrow_cell_mut(&host.pending_writes, |pending| {
                record_pending_write(
                    pending,
                    id,
                    identity,
                    bits.clone(),
                    mask.clone(),
                    domain_kind,
                    old_level,
                    new_level,
                );
            });
        }
        true
    })
    .unwrap_or(false);
    if succeeded {
        if let Some((deposited_value, deposited_mask)) = force_deposited {
            borrow_cell_mut(&host.forced_values, |forced| {
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
    succeeded && (callbacks::is_running_for_host(host) || flush_pending_writes_for_host(host))
}

fn has_inertial_writes() -> bool {
    with_host(|host| !host.inertial_writes.borrow().is_empty())
}

/// Apply one inertial update round after the current callbacks and domains.
/// A callback on an edge generated here can enqueue the next round, but its
/// deposits must not change the values sampled by that edge's FF kernel.
fn flush_inertial_writes() -> bool {
    with_host(flush_inertial_writes_for_host)
}

fn flush_inertial_writes_for_host(host: &VpiHost) -> bool {
    let writes = std::mem::take(&mut *host.inertial_writes.borrow_mut());
    if writes.is_empty() {
        return false;
    }
    for (identity, deposit) in writes {
        if !apply_signal_value(
            host,
            deposit.id,
            identity,
            deposit.value,
            deposit.mask,
            VPI_NO_DELAY,
        ) {
            callbacks::fail_for_host(host, "failed to apply an inertial VPI write".into());
            return true;
        }
    }
    flush_pending_writes_for_host(host);
    true
}

fn flush_pending_writes_for_host(host: &VpiHost) -> bool {
    let pending = borrow_cell_mut(&host.pending_writes, std::mem::take);
    if !pending.settle {
        return true;
    }
    let mut final_deposits = pending.deposits;
    let mut replay_overrides: HashMap<VpiSignalIdentity, PendingDeposit> = HashMap::default();
    for mut batch in pending.edge_batches {
        for (id, value) in &replay_overrides {
            batch.deposits.insert(*id, value.clone());
        }
        let overridden = replay_overrides.keys().copied().collect::<HashSet<_>>();
        let running = callbacks::is_running_for_host(host);
        let result: Result<Vec<celox_design::StateAddr>, celox::RuntimeErrorCode> =
            with_instance_mut(host, |runtime| {
                let active_edges = batch
                    .active_signals
                    .into_iter()
                    .filter_map(|identity| {
                        let deposit = batch.deposits.get(&identity)?;
                        let signal = deposit.id.resolve(runtime)?;
                        if overridden.contains(&identity) {
                            let (old_value, old_mask) = runtime.get_four_state(signal.signal);
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
                let context = RuntimeFormatContext {
                    tb_time: Some(callbacks::current_time_for_host(host)),
                    scope: None,
                };
                if running {
                    runtime.eval_comb_with_context(context)?;
                    Ok(active_edges)
                } else {
                    runtime.settle_active_edges_with_context(&active_edges, context)?;
                    Ok(Vec::new())
                }
            })
            .unwrap_or(Ok(Vec::new()));
        let active_edges = match result {
            Ok(active_edges) => active_edges,
            Err(error) => {
                callbacks::fail_for_host(host, error.to_string());
                return false;
            }
        };
        process_runtime_events_for_host(host);
        if callbacks::has_error_for_host(host) {
            return false;
        }
        if running {
            // Input-edge callbacks observe the state before any FF runs. Keep
            // the compiled evaluate/apply kernel fused: the observation point
            // is at its entry, not between sampling and committing every FF.
            callbacks::dispatch_value_changes_for_host(host);
            if callbacks::finish_requested_for_host(host) {
                return true;
            }
            let wrote_inputs = borrow_cell(&host.pending_writes, |pending| pending.settle);
            let result = with_instance_mut(host, |runtime| {
                let context = RuntimeFormatContext {
                    tb_time: Some(callbacks::current_time_for_host(host)),
                    scope: None,
                };
                if wrote_inputs {
                    runtime.eval_comb_with_context(context)?;
                }
                runtime.apply_active_edges_with_context(&active_edges, context)
            })
            .unwrap_or(Ok(()));
            if let Err(error) = result {
                callbacks::fail_for_host(host, error.to_string());
                return false;
            }
            process_runtime_events_for_host(host);
            if callbacks::has_error_for_host(host) || callbacks::finish_requested_for_host(host) {
                return !callbacks::has_error_for_host(host);
            }
            callbacks::dispatch_value_changes_for_host(host);
            if callbacks::finish_requested_for_host(host) {
                return true;
            }
            let mut callback_iterations = 0usize;
            loop {
                let callback_pending = borrow_cell_mut(&host.pending_writes, std::mem::take);
                if !callback_pending.settle {
                    break;
                }
                callback_iterations += 1;
                if callback_iterations > 1_000_000 {
                    callbacks::fail_for_host(
                        host,
                        "VPI callback writes exceeded 1000000 intermediate settlements".to_string(),
                    );
                    return false;
                }
                let overrides = callback_pending.deposits.clone();
                borrow_cell_mut(&host.pending_writes, |pending| *pending = callback_pending);
                if !flush_pending_writes_for_host(host) {
                    return false;
                }
                if callbacks::finish_requested_for_host(host) {
                    return true;
                }
                replay_overrides.extend(overrides.clone());
                final_deposits.extend(overrides);
                callbacks::dispatch_value_changes_for_host(host);
                if callbacks::finish_requested_for_host(host) {
                    return true;
                }
            }
        }
    }
    let result: Result<(), celox::RuntimeErrorCode> = with_instance_mut(host, |runtime| {
        apply_pending_deposits(runtime, &final_deposits);
        runtime.settle_active_edges_with_context(
            &[],
            RuntimeFormatContext {
                tb_time: Some(callbacks::current_time_for_host(host)),
                scope: None,
            },
        )
    })
    .unwrap_or(Ok(()));
    if let Err(error) = result {
        callbacks::fail_for_host(host, error.to_string());
        return false;
    }
    process_runtime_events_for_host(host);
    !callbacks::has_error_for_host(host)
}

fn process_runtime_events() {
    with_host(process_runtime_events_for_host)
}

fn process_runtime_events_for_host(host: &VpiHost) {
    let time = callbacks::current_time_for_host(host);
    let events = borrow_cell_mut(&host.runtime, |runtime| {
        runtime
            .as_mut()
            .map(|runtime| runtime.drain_runtime_events(time))
    })
    .unwrap_or_default();
    for event in events {
        match event {
            RuntimeEvent::Display { message } => {
                let mut output = std::io::stdout().lock();
                let _ = output.write_all(&message);
                let _ = output.write_all(b"\n");
            }
            RuntimeEvent::Write { message } => {
                let _ = std::io::stdout().lock().write_all(&message);
            }
            RuntimeEvent::Report(report) => {
                let mut output = std::io::stderr().lock();
                let _ = output.write_all(&report.render());
                let _ = output.write_all(b"\n");
                if report.severity == Severity::Fatal {
                    callbacks::fail_for_host(host, celox_message_diagnostic(&report.message));
                }
            }
            // Like `vpiStop` in `vpi_control`, `$stop` ends a run that has
            // no interactive mode.
            RuntimeEvent::Finish | RuntimeEvent::Stop => callbacks::request_finish_for_host(host),
            RuntimeEvent::Missed { count } => {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "celox-vpi: missed {count} runtime events"
                );
                callbacks::fail_for_host(
                    host,
                    format!(
                        "missed {count} runtime events; a fatal assertion may have been overwritten"
                    ),
                );
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

fn celox_message_diagnostic(message: &[u8]) -> String {
    std::str::from_utf8(message)
        .map(str::to_owned)
        .unwrap_or_else(|_| message.escape_ascii().to_string())
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn scheduler_unwind_releases_the_host_registration() {
        let host = std::rc::Rc::new(VpiHost::default());
        *ACTIVE_HOST.lock().unwrap() = Some(HostRegistration {
            owner: std::thread::current().id(),
            address: std::rc::Rc::into_raw(host.clone()) as usize,
        });
        let session = VpiSession {
            host: host.clone(),
            registered: true,
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _cleanup = CallbackRunCleanup;
            host.callbacks.borrow_mut().running = true;
            panic!("exercise scheduler unwind cleanup");
        }));
        assert!(result.is_err());
        assert!(!host_is_active());
        assert!(!host.callbacks.borrow().running);
        drop(session);
    }
}
