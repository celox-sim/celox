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
    ptr,
};

use celox::{
    BigUint, NativeProgramInstance, ReflectionScopeId, ReflectionSignalId, SignalDirection,
    SimBackend,
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
pub const VPI_MODULE: i32 = 32;
pub const VPI_NET: i32 = 36;
pub const VPI_PORT: i32 = 44;
pub const VPI_REG: i32 = 48;
pub const VPI_PARENT: i32 = 81;
pub const VPI_SCOPE: i32 = 84;
pub const VPI_INTERNAL_SCOPE: i32 = 92;
pub const VPI_VARIABLES: i32 = 100;

pub const VPI_TYPE: i32 = 1;
pub const VPI_NAME: i32 = 2;
pub const VPI_FULL_NAME: i32 = 3;
pub const VPI_SIZE: i32 = 4;
pub const VPI_TOP_MODULE: i32 = 7;
pub const VPI_DEF_NAME: i32 = 9;
pub const VPI_SCALAR: i32 = 17;
pub const VPI_VECTOR: i32 = 18;
pub const VPI_DIRECTION: i32 = 20;

pub const VPI_INPUT: i32 = 1;
pub const VPI_OUTPUT: i32 = 2;
pub const VPI_INOUT: i32 = 3;
pub const VPI_NO_DIRECTION: i32 = 5;

pub const VPI_NO_DELAY: i32 = 1;

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
}

/// Install an instance explicitly. This is primarily useful to an embedding
/// runtime which has already selected the attached image.
pub fn install_runtime(instance: NativeProgramInstance) {
    RUNTIME.with_borrow_mut(|runtime| *runtime = Some(instance));
}

pub fn clear_runtime() {
    RUNTIME.with_borrow_mut(|runtime| *runtime = None);
}

#[unsafe(no_mangle)]
pub extern "C" fn celox_vpi_load_current_executable() -> i32 {
    match NativeProgramInstance::from_current_executable() {
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
        HandleKind::Iterator { .. } => None,
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
            (VPI_MODULE, None) => vec![ObjectRef::Scope(ReflectionScopeId(0))],
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
                                    VPI_REG | VPI_VARIABLES => {
                                        signal.direction == SignalDirection::Internal
                                    }
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
/// `iterator` must be a live iterator handle returned by [`vpi_iterate`]. It
/// must not be reused after this function returns null.
pub unsafe extern "C" fn vpi_scan(iterator: VpiHandle) -> VpiHandle {
    // Safety: iterator follows the VPI handle contract.
    let Some(iterator_ref) = (unsafe { handle_mut(iterator) }) else {
        return ptr::null_mut();
    };
    let HandleKind::Iterator { objects, next } = &mut iterator_ref.kind else {
        return ptr::null_mut();
    };
    let Some(object) = objects.get(*next).copied() else {
        // VPI iterators are consumed when scan reaches the end.
        // Safety: this pointer came from `Box::into_raw` and is no longer used.
        drop(unsafe { Box::from_raw(iterator) });
        return ptr::null_mut();
    };
    *next += 1;
    new_object_handle(object)
}

fn object_type(object: ObjectRef) -> i32 {
    match object {
        ObjectRef::Scope(_) => VPI_MODULE,
        ObjectRef::Signal(id) => with_runtime(|runtime| {
            runtime
                .reflection()
                .signal(id)
                .map(|signal| {
                    if signal.direction == SignalDirection::Internal {
                        VPI_REG
                    } else {
                        VPI_PORT
                    }
                })
                .unwrap_or(0)
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
    // Safety: reference follows the VPI handle contract.
    let Some(object) = (unsafe { object_ref(reference) }) else {
        return 0;
    };
    match property {
        VPI_TYPE => object_type(object),
        VPI_TOP_MODULE => i32::from(object == ObjectRef::Scope(ReflectionScopeId(0))),
        VPI_SIZE | VPI_SCALAR | VPI_VECTOR | VPI_DIRECTION => with_runtime(|runtime| {
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
                VPI_DIRECTION => match signal.direction {
                    SignalDirection::Input => VPI_INPUT,
                    SignalDirection::Output => VPI_OUTPUT,
                    SignalDirection::Inout => VPI_INOUT,
                    SignalDirection::Internal => VPI_NO_DIRECTION,
                },
                _ => 0,
            }
        })
        .unwrap_or(0),
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
        _ => ptr::null_mut(),
    }
}

fn value_bits(id: ReflectionSignalId) -> Option<(BigUint, BigUint, usize)> {
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
                'x'
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
                'x'
            } else {
                let digit = (0..4).fold(0u8, |digit, offset| {
                    digit | (u8::from(value.bit((bit + offset) as u64)) << offset)
                });
                char::from_digit(u32::from(digit), 16).unwrap()
            }
        })
        .collect()
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
    let HandleKind::Object(ObjectRef::Signal(id)) = handle.kind else {
        return;
    };
    let Some((bits, mask, width)) = value_bits(id) else {
        return;
    };
    // Safety: checked non-null above; caller owns the `VpiValue` allocation.
    let value = unsafe { &mut *value };
    match value.format {
        VPI_INT_VAL => {
            value.value.integer = bits.to_u32_digits().first().copied().unwrap_or(0) as i32
        }
        VPI_SCALAR_VAL => {
            value.value.scalar = if mask.bit(0) {
                VPI_X
            } else if bits.bit(0) {
                VPI_1
            } else {
                VPI_0
            };
        }
        VPI_BIN_STR_VAL => {
            handle.value_string = Some(c_string(&format_binary(&bits, &mask, width)));
            value.value.str_ = handle.value_string.as_ref().unwrap().as_ptr().cast_mut();
        }
        VPI_HEX_STR_VAL => {
            let string = format_hex(&bits, &mask, width);
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

unsafe fn decode_value(value: *const VpiValue, width: usize) -> Option<(BigUint, BigUint)> {
    if value.is_null() {
        return None;
    }
    // Safety: checked non-null above; caller owns the `VpiValue` allocation.
    let value = unsafe { &*value };
    match value.format {
        VPI_INT_VAL => {
            // Safety: reading the active union member selected by `format`.
            Some((
                BigUint::from(unsafe { value.value.integer } as u32),
                0u8.into(),
            ))
        }
        VPI_SCALAR_VAL => {
            // Safety: reading the active union member selected by `format`.
            match unsafe { value.value.scalar } {
                VPI_0 => Some((0u8.into(), 0u8.into())),
                VPI_1 => Some((1u8.into(), 0u8.into())),
                VPI_X | VPI_Z => Some((1u8.into(), 1u8.into())),
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
                    b'x' | b'X' | b'z' | b'Z' => {
                        let unknown = (1u8 << radix_bits) - 1;
                        bits |= BigUint::from(unknown);
                        mask |= BigUint::from(unknown);
                    }
                    b'_' => {}
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
    _when: *const c_void,
    flags: i32,
) -> VpiHandle {
    if flags != VPI_NO_DELAY {
        return ptr::null_mut();
    }
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
    // Safety: value follows the VPI value contract.
    let Some((bits, mask)) = width.and_then(|width| unsafe { decode_value(value, width) }) else {
        return ptr::null_mut();
    };
    let succeeded = with_runtime_mut(|runtime| {
        let Some(signal) = runtime.reflection().signal(id).map(|signal| signal.signal) else {
            return false;
        };
        runtime.backend_mut().set_four_state(signal, bits, mask);
        runtime.eval_comb().is_ok()
    })
    .unwrap_or(false);
    if succeeded {
        reference
    } else {
        ptr::null_mut()
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
    // Safety: the handle came from `Box::into_raw` and ownership returns here.
    drop(unsafe { Box::from_raw(reference) });
    1
}
