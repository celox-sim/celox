#![cfg(feature = "host-runtime")]

use std::ffi::c_void;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use celox::{FrontendDiagnostic, Simulator, SimulatorErrorKind, TestResult};
use veryl_component_sys as sys;

struct MethodState {
    value: u64,
    api: *const sys::VrlHostApi,
}

unsafe extern "C" fn create(_ctx: *mut sys::VrlCtx, api: *const sys::VrlHostApi) -> *mut c_void {
    Box::into_raw(Box::new(MethodState { value: 0, api })).cast()
}

unsafe extern "C" fn destroy(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state.cast::<MethodState>()) });
    }
}

unsafe extern "C" fn hook(_state: *mut c_void, _ctx: *mut sys::VrlCtx) -> i32 {
    0
}

unsafe fn write_return(ret: *mut sys::VrlValue, value: u64, width: u32) {
    let ret = unsafe { &mut *ret };
    if ret.words.is_null() || ret.nwords == 0 {
        return;
    }
    unsafe { *ret.words.cast_mut() = value };
    ret.kind = sys::VRL_VALUE_BITS;
    ret.width = width;
    ret.nwords = 1;
}

unsafe fn write_wide_return(ret: *mut sys::VrlValue) {
    let ret = unsafe { &mut *ret };
    if ret.words.is_null() || ret.nwords < 2 {
        return;
    }
    let words = unsafe { std::slice::from_raw_parts_mut(ret.words.cast_mut(), ret.nwords) };
    words[0] = 1;
    words[1] = 2;
    ret.kind = sys::VRL_VALUE_BITS;
    ret.width = 96;
    ret.nwords = 2;
}

unsafe fn write_string_return(ret: *mut sys::VrlValue) {
    let ret = unsafe { &mut *ret };
    ret.kind = sys::VRL_VALUE_STRING;
    ret.width = 0;
    ret.nwords = 0;
    ret.str_ = sys::VrlStr::from_str("not bits");
}

unsafe extern "C" fn call_method(
    state: *mut c_void,
    _ctx: *mut sys::VrlCtx,
    name: sys::VrlStr,
    args: *const sys::VrlValue,
    nargs: usize,
    ret: *mut sys::VrlValue,
) -> i32 {
    let state = unsafe { &mut *state.cast::<MethodState>() };
    let name = unsafe { name.as_str() };
    let args = if nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
    match name {
        "set" if args.len() == 1 && args[0].kind == sys::VRL_VALUE_BITS => {
            state.value = unsafe { args[0].words.as_ref() }
                .copied()
                .unwrap_or_default();
            0
        }
        "set_str" if args.len() == 1 && args[0].kind == sys::VRL_VALUE_STRING => {
            if unsafe { args[0].str_.as_str() } == "hello" {
                state.value = 7;
                0
            } else {
                1
            }
        }
        "set_wide"
            if args.len() == 1
                && args[0].kind == sys::VRL_VALUE_BITS
                && args[0].width == 96
                && args[0].nwords >= 2 =>
        {
            let words = unsafe { std::slice::from_raw_parts(args[0].words, args[0].nwords) };
            if words[0] == 1 && words[1] == 2 {
                state.value = 55;
                0
            } else {
                1
            }
        }
        "set_signed"
            if args.len() == 1 && args[0].kind == sys::VRL_VALUE_BITS && args[0].width == 8 =>
        {
            if unsafe { args[0].words.as_ref() }
                .copied()
                .unwrap_or_default()
                & 0xff
                == 0xff
            {
                state.value = 77;
                0
            } else {
                1
            }
        }
        "get" if args.is_empty() => {
            unsafe { write_return(ret, state.value, 8) };
            0
        }
        "bump" if args.is_empty() => {
            state.value = state.value.wrapping_add(1);
            unsafe { write_return(ret, state.value, 8) };
            0
        }
        "set_pair" if args.len() == 2 => {
            let first = unsafe { args[0].words.as_ref() }
                .copied()
                .unwrap_or_default();
            let second = unsafe { args[1].words.as_ref() }
                .copied()
                .unwrap_or_default();
            state.value = first * 10 + second;
            0
        }
        "time" if args.is_empty() => {
            let time = unsafe { ((*state.api).sim_time)(_ctx) };
            unsafe { write_return(ret, time, 64) };
            0
        }
        "save" if args.len() == 1 && args[0].kind == sys::VRL_VALUE_STRING => {
            let path = unsafe { args[0].str_.as_str() };
            let handle = unsafe {
                ((*state.api).file_open)(_ctx, sys::VrlStr::from_str(path), sys::VRL_FILE_CREATE)
            };
            if handle < 0 {
                return 1;
            }
            let bytes = state.value.to_le_bytes();
            let written =
                unsafe { ((*state.api).file_write)(_ctx, handle, bytes.as_ptr(), bytes.len()) };
            unsafe { ((*state.api).file_close)(_ctx, handle) };
            i32::from(written != bytes.len() as i64)
        }
        "load" if args.len() == 1 && args[0].kind == sys::VRL_VALUE_STRING => {
            let path = unsafe { args[0].str_.as_str() };
            let handle = unsafe {
                ((*state.api).file_open)(_ctx, sys::VrlStr::from_str(path), sys::VRL_FILE_READ)
            };
            if handle < 0 {
                return 1;
            }
            let mut bytes = [0; 8];
            let read =
                unsafe { ((*state.api).file_read)(_ctx, handle, bytes.as_mut_ptr(), bytes.len()) };
            unsafe { ((*state.api).file_close)(_ctx, handle) };
            if read != bytes.len() as i64 {
                return 1;
            }
            state.value = u64::from_le_bytes(bytes);
            0
        }
        "report_fail" if args.is_empty() => {
            unsafe { ((*state.api).fail)(_ctx, sys::VrlStr::from_str("reported failure")) };
            0
        }
        "lying" if args.is_empty() => {
            unsafe { write_return(ret, state.value, 16) };
            0
        }
        "wide" if args.is_empty() => {
            unsafe { write_wide_return(ret) };
            0
        }
        "wide_declared" if args.is_empty() => {
            unsafe { write_wide_return(ret) };
            0
        }
        "string_return" if args.is_empty() => {
            unsafe { write_string_return(ret) };
            0
        }
        "unit" if args.is_empty() => 0,
        _ => 1,
    }
}

static COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_METHOD_ONLY,
    create,
    destroy,
    on_init: hook,
    on_reset: hook,
    on_clock: hook,
    call_method,
    on_finish: hook,
};

struct ClockState {
    api: *const sys::VrlHostApi,
    input: u32,
    output: u32,
    trace: i32,
    step: u64,
    clocks: u64,
}

unsafe extern "C" fn create_clock(
    ctx: *mut sys::VrlCtx,
    api: *const sys::VrlHostApi,
) -> *mut c_void {
    let api_ref = unsafe { &*api };
    let clock =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("clk"), sys::VRL_DIR_CLOCK) };
    let input =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("d"), sys::VRL_DIR_INPUT) };
    let output =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("q"), sys::VRL_DIR_OUTPUT) };
    let trace = unsafe { (api_ref.trace_var)(ctx, sys::VrlStr::from_str("state"), 8) };
    let mut step = sys::VrlValue::unit();
    let has_step =
        unsafe { (api_ref.param_get)(ctx, sys::VrlStr::from_str("STEP"), &mut step) } == 0;
    if clock < 0 || input < 0 || output < 0 {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(ClockState {
        api,
        input: input as u32,
        output: output as u32,
        trace,
        step: if has_step {
            unsafe { step.words.as_ref() }.copied().unwrap_or_default()
        } else {
            0
        },
        clocks: 0,
    }))
    .cast()
}

unsafe extern "C" fn create_bad_clock_role(
    ctx: *mut sys::VrlCtx,
    api: *const sys::VrlHostApi,
) -> *mut c_void {
    let api_ref = unsafe { &*api };
    let clock =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("clk"), sys::VRL_DIR_INPUT) };
    let input =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("d"), sys::VRL_DIR_INPUT) };
    let output =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("q"), sys::VRL_DIR_OUTPUT) };
    if clock < 0 || input < 0 || output < 0 {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(ClockState {
        api,
        input: input as u32,
        output: output as u32,
        trace: -1,
        step: 1,
        clocks: 0,
    }))
    .cast()
}

unsafe extern "C" fn destroy_clock(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state.cast::<ClockState>()) });
    }
}

unsafe extern "C" fn clock_hook(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &mut *state.cast::<ClockState>() };
    state.clocks = state.clocks.saturating_add(1);
    let api = unsafe { &*state.api };
    let mut word = 0;
    let mut mask_xz = 0;
    unsafe { (api.read_input)(ctx, state.input, &mut word, &mut mask_xz) };
    word = word.wrapping_add(state.step);
    unsafe { (api.write_output)(ctx, state.output, &word, &mask_xz) };
    if state.trace >= 0 {
        unsafe { (api.trace_write)(ctx, state.trace, &word) };
    }
    0
}

unsafe extern "C" fn clock_call_method(
    state: *mut c_void,
    ctx: *mut sys::VrlCtx,
    name: sys::VrlStr,
    args: *const sys::VrlValue,
    nargs: usize,
    _ret: *mut sys::VrlValue,
) -> i32 {
    let state = unsafe { &mut *state.cast::<ClockState>() };
    let api = unsafe { &*state.api };
    let name = unsafe { name.as_str() };
    let args = if nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args, nargs) }
    };
    let argument = |index: usize| {
        args.get(index)
            .and_then(|arg| unsafe { arg.words.as_ref() })
            .copied()
    };
    match name {
        "unit" if args.is_empty() => 0,
        "check_input" if args.len() == 1 => {
            let mut value = 0;
            let mut mask_xz = 0;
            unsafe { (api.read_input)(ctx, state.input, &mut value, &mut mask_xz) };
            if Some(value) == argument(0) && mask_xz == 0 {
                0
            } else {
                unsafe {
                    (api.fail)(
                        ctx,
                        sys::VrlStr::from_str("component method observed stale input"),
                    )
                };
                1
            }
        }
        "drive" if args.len() == 1 => {
            let value = argument(0).unwrap_or_default();
            unsafe { (api.write_output)(ctx, state.output, &value, std::ptr::null()) };
            0
        }
        "check_clocks" if args.len() == 1 && argument(0) == Some(state.clocks) => 0,
        "stop" if args.is_empty() => {
            unsafe { (api.finish)(ctx) };
            0
        }
        _ => 1,
    }
}

unsafe extern "C" fn clock_init(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &mut *state.cast::<ClockState>() };
    let api = unsafe { &*state.api };
    let word = 0x33;
    unsafe { (api.write_output)(ctx, state.output, &word, std::ptr::null()) };
    0
}

static CLOCK_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_CLOCKED,
    create: create_clock,
    destroy: destroy_clock,
    on_init: clock_init,
    on_reset: hook,
    on_clock: clock_hook,
    call_method: clock_call_method,
    on_finish: hook,
};

struct WideClockState {
    api: *const sys::VrlHostApi,
    input: u32,
    output: u32,
    words: usize,
}

unsafe extern "C" fn create_wide_clock(
    ctx: *mut sys::VrlCtx,
    api: *const sys::VrlHostApi,
) -> *mut c_void {
    let api_ref = unsafe { &*api };
    let clock =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("clk"), sys::VRL_DIR_CLOCK) };
    let input =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("d"), sys::VRL_DIR_INPUT) };
    let output =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("q"), sys::VRL_DIR_OUTPUT) };
    if clock < 0 || input < 0 || output < 0 {
        return std::ptr::null_mut();
    }
    let width = unsafe { (api_ref.port_width)(ctx, input as u32) } as usize;
    if unsafe { (api_ref.port_width)(ctx, output as u32) } as usize != width {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(WideClockState {
        api,
        input: input as u32,
        output: output as u32,
        words: width.div_ceil(64).max(1),
    }))
    .cast()
}

unsafe extern "C" fn destroy_wide_clock(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state.cast::<WideClockState>()) });
    }
}

unsafe extern "C" fn wide_clock_hook(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &mut *state.cast::<WideClockState>() };
    let api = unsafe { &*state.api };
    let mut words = vec![0; state.words];
    let mut mask_xz = vec![0; state.words];
    unsafe { (api.read_input)(ctx, state.input, words.as_mut_ptr(), mask_xz.as_mut_ptr()) };
    unsafe { (api.write_output)(ctx, state.output, words.as_ptr(), mask_xz.as_ptr()) };
    0
}

static WIDE_CLOCK_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_CLOCKED,
    create: create_wide_clock,
    destroy: destroy_wide_clock,
    on_init: hook,
    on_reset: hook,
    on_clock: wide_clock_hook,
    call_method,
    on_finish: hook,
};

struct ModportState {
    api: *const sys::VrlHostApi,
    ready: u32,
    valid: u32,
}

unsafe extern "C" fn create_modport(
    ctx: *mut sys::VrlCtx,
    api: *const sys::VrlHostApi,
) -> *mut c_void {
    let api_ref = unsafe { &*api };
    let clock =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("clk"), sys::VRL_DIR_CLOCK) };
    let ready = unsafe {
        (api_ref.port_index)(ctx, sys::VrlStr::from_str("bus.ready"), sys::VRL_DIR_INPUT)
    };
    let valid = unsafe {
        (api_ref.port_index)(ctx, sys::VrlStr::from_str("bus.valid"), sys::VRL_DIR_OUTPUT)
    };
    if clock < 0 || ready < 0 || valid < 0 {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(ModportState {
        api,
        ready: ready as u32,
        valid: valid as u32,
    }))
    .cast()
}

unsafe extern "C" fn destroy_modport(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state.cast::<ModportState>()) });
    }
}

unsafe extern "C" fn modport_clock_hook(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &mut *state.cast::<ModportState>() };
    let api = unsafe { &*state.api };
    let mut value = 0;
    let mut mask_xz = 0;
    unsafe { (api.read_input)(ctx, state.ready, &mut value, &mut mask_xz) };
    unsafe { (api.write_output)(ctx, state.valid, &value, &mask_xz) };
    0
}

static MODPORT_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_CLOCKED,
    create: create_modport,
    destroy: destroy_modport,
    on_init: hook,
    on_reset: hook,
    on_clock: modport_clock_hook,
    call_method,
    on_finish: hook,
};

static BAD_CLOCK_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_CLOCKED,
    create: create_bad_clock_role,
    destroy: destroy_clock,
    on_init: hook,
    on_reset: hook,
    on_clock: clock_hook,
    call_method,
    on_finish: hook,
};

struct ResetState {
    api: *const sys::VrlHostApi,
    output: u32,
    resets: u64,
    clocks: u64,
}

unsafe extern "C" fn create_reset(
    ctx: *mut sys::VrlCtx,
    api: *const sys::VrlHostApi,
) -> *mut c_void {
    let api_ref = unsafe { &*api };
    let clock =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("clk"), sys::VRL_DIR_CLOCK) };
    let reset =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("rst"), sys::VRL_DIR_RESET) };
    let output =
        unsafe { (api_ref.port_index)(ctx, sys::VrlStr::from_str("q"), sys::VRL_DIR_OUTPUT) };
    if clock < 0 || reset < 0 || output < 0 {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(ResetState {
        api,
        output: output as u32,
        resets: 0,
        clocks: 0,
    }))
    .cast()
}

unsafe extern "C" fn destroy_reset(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state.cast::<ResetState>()) });
    }
}

unsafe extern "C" fn reset_hook(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &mut *state.cast::<ResetState>() };
    state.resets += 1;
    let api = unsafe { &*state.api };
    unsafe { (api.write_output)(ctx, state.output, &state.resets, std::ptr::null()) };
    0
}

unsafe extern "C" fn reset_clock_hook(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &mut *state.cast::<ResetState>() };
    state.clocks += 1;
    let value = 100 + state.clocks;
    let api = unsafe { &*state.api };
    unsafe { (api.write_output)(ctx, state.output, &value, std::ptr::null()) };
    0
}

static RESET_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_CLOCKED,
    create: create_reset,
    destroy: destroy_reset,
    on_init: hook,
    on_reset: reset_hook,
    on_clock: reset_clock_hook,
    call_method,
    on_finish: hook,
};

struct FinishState {
    api: *const sys::VrlHostApi,
}

unsafe extern "C" fn create_finisher(
    ctx: *mut sys::VrlCtx,
    api: *const sys::VrlHostApi,
) -> *mut c_void {
    let clock =
        unsafe { ((*api).port_index)(ctx, sys::VrlStr::from_str("clk"), sys::VRL_DIR_CLOCK) };
    if clock < 0 {
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(FinishState { api })).cast()
}

unsafe extern "C" fn destroy_finisher(state: *mut c_void) {
    if !state.is_null() {
        drop(unsafe { Box::from_raw(state.cast::<FinishState>()) });
    }
}

unsafe extern "C" fn finish_clock_hook(state: *mut c_void, ctx: *mut sys::VrlCtx) -> i32 {
    let state = unsafe { &*state.cast::<FinishState>() };
    unsafe { ((*state.api).finish)(ctx) };
    0
}

static FINISH_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_CLOCKED,
    create: create_finisher,
    destroy: destroy_finisher,
    on_init: hook,
    on_reset: hook,
    on_clock: finish_clock_hook,
    call_method,
    on_finish: hook,
};

static CLEANUP_DROPS: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn destroy_cleanup(state: *mut c_void) {
    if !state.is_null() {
        CLEANUP_DROPS.fetch_add(1, Ordering::Relaxed);
        drop(unsafe { Box::from_raw(state.cast::<MethodState>()) });
    }
}

unsafe extern "C" fn create_failure(
    _ctx: *mut sys::VrlCtx,
    _api: *const sys::VrlHostApi,
) -> *mut c_void {
    std::ptr::null_mut()
}

static CLEANUP_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_METHOD_ONLY,
    create,
    destroy: destroy_cleanup,
    on_init: hook,
    on_reset: hook,
    on_clock: hook,
    call_method,
    on_finish: hook,
};

static FAILING_COMPONENT: sys::VrlComponentVTable = sys::VrlComponentVTable {
    abi_version: sys::VRL_COMPONENT_ABI_VERSION,
    kind: sys::VRL_KIND_METHOD_ONLY,
    create: create_failure,
    destroy,
    on_init: hook,
    on_reset: hook,
    on_clock: hook,
    call_method,
    on_finish: hook,
};

fn register_component() {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        celox::register_static_component("celox_counter", &COMPONENT);
        celox::register_static_component("celox_unchecked", &COMPONENT);
        celox::register_static_component("celox_param", &COMPONENT);
        celox::register_static_component("celox_clocked", &CLOCK_COMPONENT);
        celox::register_static_component("celox_wide_clocked", &WIDE_CLOCK_COMPONENT);
        celox::register_static_component("celox_modport", &MODPORT_COMPONENT);
        celox::register_static_component("celox_bad_clock", &BAD_CLOCK_COMPONENT);
        celox::register_static_component("celox_reset", &RESET_COMPONENT);
        celox::register_static_component("celox_finisher", &FINISH_COMPONENT);
        celox::register_static_component("celox_cleanup", &CLEANUP_COMPONENT);
        celox::register_static_component("celox_create_failure", &FAILING_COMPONENT);
    });
}

fn component_metadata() -> (tempfile::TempDir, veryl_metadata::Metadata) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("component")).unwrap();
    std::fs::write(
        dir.path().join("Veryl.toml"),
        r#"
            [project]
            name = "component_test"
            version = "0.1.0"

            [[components]]
            path = "component"
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("component/veryl.manifest.json"),
        r#"{
            "types": {
                "celox_counter": {
                    "kind": "method_only",
                    "methods": [
                        {"name":"set","args":[{"name":"value","type":"value"}]},
                        {"name":"set_str","args":[{"name":"value","type":"string"}]},
                        {"name":"set_wide","args":[{"name":"value","type":"value"}]},
                        {"name":"set_signed","args":[{"name":"value","type":"value"}]},
                        {"name":"set_pair","args":[{"name":"first","type":"value"},{"name":"second","type":"value"}]},
                        {"name":"get","args":[],"ret":"value","ret_width":8},
                        {"name":"bump","args":[],"ret":"value","ret_width":8},
                        {"name":"time","args":[],"ret":"value","ret_width":64},
                        {"name":"save","args":[{"name":"path","type":"string"}]},
                        {"name":"load","args":[{"name":"path","type":"string"}]},
                        {"name":"lying","args":[],"ret":"value","ret_width":8},
                        {"name":"wide_declared","args":[],"ret":"value","ret_width":96}
                    ]
                },
                "celox_unchecked": {
                    "kind": "method_only"
                },
                "celox_param": {
                    "kind": "method_only",
                    "params": [{"name":"WIDTH","type":"value"}],
                    "methods": [
                        {"name":"wide_declared","args":[],"ret":"value","ret_width":"WIDTH"}
                    ]
                },
                "celox_missing": {
                    "kind": "method_only"
                },
                "celox_clocked": {
                    "kind": "clocked",
                    "ports": [
                        {"name":"clk","dir":"input","role":"clock"},
                        {"name":"d","dir":"input"},
                        {"name":"q","dir":"output"}
                    ],
                    "params": [{"name":"STEP","type":"value"}]
                },
                "celox_wide_clocked": {
                    "kind": "clocked",
                    "ports": [
                        {"name":"clk","dir":"input","role":"clock"},
                        {"name":"d","dir":"input"},
                        {"name":"q","dir":"output"}
                    ]
                },
                "celox_modport": {
                    "kind": "clocked",
                    "ports": [
                        {"name":"clk","dir":"input","role":"clock"}
                    ],
                    "groups": [{
                        "name":"bus",
                        "interface":"HsIf",
                        "modport":"master",
                        "members":[
                            {"member":"ready","dir":"input"},
                            {"member":"valid","dir":"output"},
                            {"member":"data","dir":"output"}
                        ]
                    }]
                },
                "celox_cleanup": {
                    "kind": "method_only"
                },
                "celox_create_failure": {
                    "kind": "method_only"
                },
                "celox_bad_clock": {
                    "kind": "clocked",
                    "ports": [
                        {"name":"clk","dir":"input","role":"clock"},
                        {"name":"d","dir":"input"},
                        {"name":"q","dir":"output"}
                    ]
                },
                "celox_reset": {
                    "kind": "clocked",
                    "ports": [
                        {"name":"clk","dir":"input","role":"clock"},
                        {"name":"rst","dir":"input","role":"reset"},
                        {"name":"q","dir":"output"}
                    ]
                },
                "celox_finisher": {
                    "kind": "clocked",
                    "ports": [
                        {"name":"clk","dir":"input","role":"clock"}
                    ]
                }
            }
        }"#,
    )
    .unwrap();
    let metadata = veryl_metadata::Metadata::load(dir.path().join("Veryl.toml")).unwrap();
    (dir, metadata)
}

#[test]
fn partial_component_initialization_cleans_up_created_instances() {
    register_component();
    CLEANUP_DROPS.store(0, Ordering::Relaxed);
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var created: $comp::celox_cleanup;
            var failing: $comp::celox_create_failure;
            initial {
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected component creation failure");
    };
    assert!(message.contains("failed to initialize"), "{message}");
    assert_eq!(CLEANUP_DROPS.load(Ordering::Relaxed), 1);
}

#[test]
fn duplicate_component_instance_identity_is_rejected() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var duplicate: $comp::celox_counter;
            var duplicate: $comp::celox_counter;
            initial {
                $finish();
            }
        }
    "#;

    let error = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .unwrap_err();
    assert!(matches!(error.kind(), SimulatorErrorKind::Analyzer(_)));
}

#[test]
fn connected_clocked_component_stages_inputs_and_applies_outputs() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            var expected: logic<8>;
            var armed: logic;
            inst component: $comp::celox_clocked #(STEP: 1) (clk: clk, d, q);

            always_ff (clk) {
                expected = d + 1;
                armed = 1;
            }

            always_comb {
                if armed {
                    $assert(q == expected, "component output must settle with the FF edge");
                }
            }

            initial {
                $assert(q == 8'h33, "on_init output");
                d = 8'h29;
                clk.next();
                $assert(q == 8'h2a, "component output after first edge: %h", q);
                d = 8'h09;
                clk.next();
                $assert(q == 8'h0a, "component observes the current input: %h", q);
                $finish();
            }
        }
    "#;

    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .unwrap();
    let program = simulator.program().testbench.as_ref().unwrap();
    let component = &program.components()[0];
    assert!(component.connections.iter().any(|port| port.is_clock));
    assert!(component.connections.iter().any(|port| port.has_output));
    assert!(component.source.is_some());
    assert!(matches!(
        component.params.as_slice(),
        [(name, celox_testbench::ComponentParameterValue::Bits { words, .. })]
            if name == "STEP" && words.first() == Some(&1)
    ));
    assert!(
        !simulator.program().runtime_schema.comb_observers.is_empty(),
        "the scheduling regression requires the comb-observer path"
    );
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(testbench.component_bindings().len(), 1);
    let result = celox::testbench::run_compiled_testbench(&mut simulator, &testbench);
    assert_eq!(result, TestResult::Pass);

    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test_cranelift()
            .unwrap(),
        TestResult::Pass
    );

    let (_dir, metadata) = component_metadata();
    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build_wasm()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn upstream_wide_component_case_passes_on_all_backends() {
    register_component();
    let code = r#"
        module WideCounter (
            clk: input clock,
            rst: input reset,
            cnt: output logic<100>,
        ) {
            always_ff {
                if_reset { cnt = 0; }
                else { cnt = cnt + 100'h4_0000_0000_0000_0001; }
            }
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<100>;
            var q: logic<100>;
            inst dut: WideCounter (clk, rst, cnt);
            inst component: $comp::celox_wide_clocked (clk, d: cnt, q);
            initial {
                rst.assert();
                clk.next();
                $assert(q == 0, "wide pre-edge value");
                clk.next();
                $assert(q == 100'h4_0000_0000_0000_0001, "wide high word");
                clk.next();
                $assert(q == 100'h8_0000_0000_0000_0002, "wide continuation");
                $finish();
            }
        }
    "#;

    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test_cranelift()
            .unwrap(),
        TestResult::Pass
    );
    let (_dir, metadata) = component_metadata();
    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build_wasm()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn upstream_modport_component_case_passes_on_all_backends() {
    register_component();
    let code = r#"
        interface HsIf {
            var ready: logic;
            var valid: logic;
            var data: logic<8>;
            modport master {
                ready: input,
                valid: output,
                data: output,
            }
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst bus: HsIf;
            inst component: $comp::celox_modport (clk, bus: bus.master);
            initial {
                bus.ready = 0;
                clk.next();
                $assert(bus.valid == 0, "not ready");
                bus.ready = 1;
                clk.next();
                clk.next();
                $assert(bus.valid == 1, "valid follows ready");
                $finish();
            }
        }
    "#;

    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test_cranelift()
            .unwrap(),
        TestResult::Pass
    );
    let (_dir, metadata) = component_metadata();
    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build_wasm()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn upstream_packed_struct_connection_flattens_across_component_abi() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            struct Pair {
                hi: logic<8>,
                lo: logic<4>,
            }
            inst clk: $tb::clock_gen;
            var pair: Pair;
            var q: logic<12>;
            inst component: $comp::celox_clocked #(STEP: 0) (clk, d: pair, q);
            initial {
                pair.hi = 8'hab;
                pair.lo = 4'h5;
                clk.next();
                $assert(q == 12'hab5, "packed struct ABI layout");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

fn run_component_four_state_roundtrip<B: celox::SimBackend>(mut simulator: Simulator<B>) {
    let input = simulator.signal("d");
    let output = simulator.signal("q");
    simulator.set_four_state(input, 0b1010_0101u8.into(), 0b0011_1100u8.into());
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert!(
        testbench.component_bindings()[0]
            .connections
            .iter()
            .find(|connection| connection.port == "d")
            .unwrap()
            .input_signal
            .is_some()
    );
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
    assert_eq!(
        simulator.get_four_state(output),
        (0b1010_0101u8.into(), 0b0011_1100u8.into())
    );
}

#[test]
fn component_ports_preserve_four_state_masks_on_all_backends() {
    register_component();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst component: $comp::celox_clocked #(STEP: 0) (clk: clk, d, q);
            initial {
                clk.next(2);
                $finish();
            }
        }
    "#;

    let (_dir, metadata) = component_metadata();
    run_component_four_state_roundtrip(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .four_state(true)
            .build()
            .unwrap(),
    );

    let (_dir, metadata) = component_metadata();
    run_component_four_state_roundtrip(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .four_state(true)
            .build_cranelift()
            .unwrap(),
    );

    let (_dir, metadata) = component_metadata();
    run_component_four_state_roundtrip(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .four_state(true)
            .build_wasm()
            .unwrap(),
    );
}

#[test]
fn component_trace_var_appears_in_celox_vcd() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let output = tempfile::tempdir().unwrap();
    let vcd_path = output.path().join("component.vcd");
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst component: $comp::celox_clocked #(STEP: 0) (clk: clk, d, q);
            initial {
                d = 8'h2a;
                clk.next();
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .vcd(&vcd_path)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
    let dump = std::fs::read_to_string(vcd_path).unwrap();
    assert!(dump.contains("$scope module component $end"), "{dump}");
    assert!(
        dump.contains("$var wire 8") && dump.contains(" state $end"),
        "{dump}"
    );
    assert!(dump.contains("b101010"), "{dump}");
}

#[test]
fn component_output_conflicting_with_rtl_driver_fails() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        module Driver (
            clk: input clock,
            q: output logic<8>,
        ) {
            always_ff (clk) { q += 1; }
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst dut: Driver (clk, q);
            inst component: $comp::celox_clocked #(STEP: 0) (clk: clk, d, q);
            initial { clk.next(); $finish(); }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected multiple-driver failure");
    };
    assert!(
        message.contains("conflicts with an RTL driver"),
        "{message}"
    );
}

#[test]
fn component_outputs_conflicting_with_each_other_fail() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var a: logic<8>;
            var b: logic<8>;
            var q: logic<8>;
            inst first: $comp::celox_clocked #(STEP: 0) (clk: clk, d: a, q);
            inst second: $comp::celox_clocked #(STEP: 0) (clk: clk, d: b, q);
            initial { clk.next(); $finish(); }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected multiple-component-driver failure");
    };
    assert!(
        message.contains("conflicts with component `first`"),
        "{message}"
    );
}

#[test]
fn component_on_gated_clock_fires_only_with_the_gate() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        module GatedCounter (
            clk: input clock,
            cnt: output logic<8>,
        ) {
            always_ff (clk) {
                cnt += 1;
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var en: logic;
            let gated_clk: '_ clock = clk & en;
            var cnt: logic<8>;
            var q: logic<8>;
            inst dut: GatedCounter (clk: gated_clk, cnt);
            inst component: $comp::celox_clocked #(STEP: 0) (
                clk: gated_clk,
                d: cnt,
                q,
            );

            initial {
                en = 0;
                clk.next(3);
                $assert(cnt == 0, "gate closed: RTL");
                $assert(q == 8'h33, "gate closed: component");
                en = 1;
                clk.next(2);
                $assert(cnt == 2, "gate open: RTL cnt=%d", cnt);
                $assert(q == 1, "component observes pre-edge RTL state q=%d", q);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn component_on_hierarchical_derived_clock_fires() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        module DivDut (
            clk: input clock,
            rst: input reset,
            cnt: output logic<8>,
        ) {
            var toggle: logic;
            always_ff (clk, rst) {
                if_reset { toggle = 0; } else { toggle = ~toggle; }
            }
            let div_clk: '_ clock = clk & toggle;
            always_ff (div_clk, rst) {
                if_reset { cnt = 0; } else { cnt += 1; }
            }
        }

        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var cnt: logic<8>;
            var q: logic<8>;
            inst dut: DivDut (clk, rst, cnt);
            inst component: $comp::celox_clocked #(STEP: 0) (
                clk: dut.div_clk,
                d: cnt,
                q,
            );

            initial {
                rst.assert();
                clk.next(10);
                $assert(cnt == 5, "divided clock ticked five times");
                $assert(q == 4, "component mirrors pre-edge divided state");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn timed_simulation_ff_derived_clock_fires_five_times() {
    let code = r#"
        module DivDut (
            clk: input clock,
            rst: input reset_async_high,
            cnt: output logic<8>,
        ) {
            var toggle: logic;
            always_ff (clk, rst) {
                if_reset { toggle = 0; } else { toggle = ~toggle; }
            }
            let div_clk: '_ clock = clk & toggle;
            always_ff (div_clk, rst) {
                if_reset { cnt = 0; } else { cnt += 1; }
            }
        }
        module Top (
            clk: input clock,
            rst: input reset_async_high,
            cnt: output logic<8>,
        ) {
            inst dut: DivDut (clk, rst, cnt);
        }
    "#;
    let mut sim = celox::Simulation::builder(code, "Top").build().unwrap();
    let cnt = sim.signal("cnt");
    sim.schedule("rst", 0, 1).unwrap();
    sim.schedule("clk", 0, 0).unwrap();
    sim.step().unwrap();
    sim.schedule("rst", 10, 0).unwrap();
    sim.step().unwrap();
    for cycle in 0..10 {
        sim.schedule("clk", 20 + cycle * 20, 1).unwrap();
        sim.step().unwrap();
        sim.schedule("clk", 30 + cycle * 20, 0).unwrap();
        sim.step().unwrap();
    }
    assert_eq!(sim.get(cnt), 5u8.into());
}

#[test]
fn clocked_inst_component_accepts_zero_time_methods() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst component: $comp::celox_clocked #(STEP: 1) (clk: clk, d, q);
            initial {
                component.unit();
                $assert(q == 8'h33, "method call preserves on_init state");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn component_method_refreshes_inputs_and_applies_outputs_immediately() {
    register_component();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<16>;
            var q: logic<16>;
            inst component: $comp::celox_clocked #(STEP: 0) (
                clk,
                d: d[7:0],
                q: q[7:0],
            );
            initial {
                d = 16'h1234;
                q = 16'hab00;
                component.check_input(8'h34);
                component.drive(8'h5a);
                $assert(q == 16'hab5a, "method output must update only the selected destination: %h", q);
                $finish();
            }
        }
    "#;

    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test_cranelift()
            .unwrap(),
        TestResult::Pass
    );
    let (_dir, metadata) = component_metadata();
    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build_wasm()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn component_method_finish_request_stops_immediately() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst component: $comp::celox_clocked #(STEP: 0) (clk, d, q);
            initial {
                component.stop();
                $assert(0, "finish requested by a method must stop before this statement");
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn reset_assert_advances_components_that_only_listen_to_the_clock() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var d: logic<8>;
            var q: logic<8>;
            inst component: $comp::celox_clocked #(STEP: 0) (clk, d, q);
            initial {
                rst.assert();
                component.check_clocks(3);
                $finish();
            }
        }
    "#;

    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn synchronous_reset_without_runtime_reset_event_still_binds() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        module SyncDut (
            clk: input clock,
            rst: input reset_sync_low,
            q: output logic<8>,
        ) {
            always_ff (clk, rst) {
                if_reset { q = 0; } else { q += 1; }
            }
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var d: logic<8>;
            var component_q: logic<8>;
            var q: logic<8>;
            inst dut: SyncDut (clk, rst, q);
            inst component: $comp::celox_clocked #(STEP: 0) (clk, d, q: component_q);
            initial {
                rst.assert();
                $assert(q == 0, "synchronous reset was applied");
                component.check_clocks(3);
                clk.next();
                $assert(q == 1, "clock advances after reset deassertion");
                $finish();
            }
        }
    "#;

    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    let semantic_reset = simulator
        .program()
        .testbench
        .as_ref()
        .unwrap()
        .statements()
        .iter()
        .find_map(|statement| match statement {
            celox_testbench::TestbenchStatement::ResetAssert {
                reset_signal,
                clock_event,
                duration,
                assert_value,
                deassert_value,
                ..
            } => Some(celox_testbench::TestbenchStatement::ResetAssert {
                reset_signal: *reset_signal,
                reset_event: None,
                clock_event: *clock_event,
                duration: duration.clone(),
                assert_value: *assert_value,
                deassert_value: *deassert_value,
            }),
            _ => None,
        })
        .unwrap();
    let isolated = celox_testbench::TestbenchProgram::new(vec![semantic_reset]);
    let bound = celox_runtime::bind_testbench_program(
        simulator.backend_ref(),
        isolated,
        &std::collections::HashSet::new(),
    )
    .unwrap();
    assert!(matches!(
        bound.statements().first(),
        Some(celox_testbench::TestbenchStatement::ResetAssert {
            reset_event: None,
            ..
        })
    ));
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn component_declared_in_nested_module_is_elaborated_and_runs() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(child)]
        module Child (
            clk: input clock,
            d: input logic<8>,
            q: output logic<8>,
        ) {
            inst component: $comp::celox_clocked #(STEP: 1) (clk, d, q);
        }
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst child: Child (clk, d, q);
            initial {
                d = 8'h29;
                clk.next();
                $assert(q == 8'h2a, "nested component output: %h", q);
                $finish();
            }
        }
    "#;

    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .unwrap();
    let program = simulator.program().testbench.as_ref().unwrap();
    assert_eq!(program.components()[0].instance, "child[0].component");
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn component_clock_role_mismatch_is_reported() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            var d: logic<8>;
            var q: logic<8>;
            inst component: $comp::celox_bad_clock (clk: clk, d, q);
            initial {
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected component clock-role failure");
    };
    assert!(
        message.contains("did not resolve `clk` as a clock port"),
        "{message}"
    );
}

#[test]
fn component_reset_hook_uses_reset_event_and_precedes_clock_hooks() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst rst: $tb::reset_gen(clk);
            var q: logic<8>;
            inst component: $comp::celox_reset (clk, rst, q);

            initial {
                rst.assert();
                $assert(q == 3, "on_reset fires for every reset cycle: %d", q);
                clk.next();
                $assert(q == 101, "on_clock follows reset hooks: %d", q);
                clk.next();
                $assert(q == 102, "on_clock keeps firing: %d", q);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn component_finish_request_stops_the_clock_loop() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            inst clk: $tb::clock_gen;
            inst component: $comp::celox_finisher (clk);
            initial {
                clk.next(10);
                $assert(0, "finish request must stop before this statement");
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn component_method_statement_and_expression_forms_roundtrip() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_counter;
            var other: $comp::celox_counter;
            var value: logic<8>;
            var argument: logic<8>;
            var signed_argument: signed logic<8>;
            var limit: logic<8>;
            var source: logic<8>;
            var derived: logic<8>;
            var observed: logic<8>;
            var time_before: logic<64>;
            var time_after: logic<64>;

            function update_in_function(next: input logic<8>) {
                component.set(next);
            }

            always_comb {
                derived = source + 1;
                observed = value + 1;
            }

            initial {
                source = 40;
                component.set(derived);
                value = component.get();
                $assert(observed == 42, "component call is a zero-time comb barrier");
                argument = 41;
                component.set(argument);
                value = component.get() + 1;
                $assert(value == 42, "expression return");
                component.set_str("hello");
                value = component.get();
                $assert(value == 7, "string argument and retained state");
                component.set_wide(96'h0000_0002_0000_0000_0000_0001);
                value = component.get();
                $assert(value == 55, "wide argument");
                signed_argument = -1;
                component.set_signed(signed_argument);
                value = component.get();
                $assert(value == 77, "signed argument encoding");
                component.set_pair(3, 4);
                value = component.get();
                $assert(value == 34, "arguments preserve source order");
                time_before = component.time();
                component.set(12);
                time_after = component.time();
                $assert(time_after == time_before, "component methods are zero-time");
                if value == 34 {
                    component.set(12);
                }
                update_in_function(13);
                value = component.get();
                $assert(value == 13, "calls nested in conditionals and functions");
                other.set(99);
                value = other.get();
                $assert(value == 99, "second instance state");
                value = component.get();
                $assert(value == 13, "instances are independent");
                limit = 3;
                for i in 0..limit {
                    other.set(i);
                }
                value = other.get();
                $assert(value == 2, "component call in dynamic loop");
                $finish();
            }
        }
    "#;

    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .opt_level(celox::OptLevel::O2)
        .build()
        .unwrap();
    let components = simulator.program().testbench.as_ref().unwrap().components();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0].instance, "component");
    assert_eq!(components[1].instance, "other");
    assert!(components.iter().all(|component| component.is_var_form));
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass,
        "a second run must create fresh component instances"
    );
}

#[test]
fn upstream_string_argument_and_host_file_service_roundtrip() {
    register_component();
    let (dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_counter;
            var value: logic<8>;
            initial {
                component.set(42);
                component.save("state.bin");
                component.set(0);
                component.load("state.bin");
                value = component.get();
                $assert(value == 42, "file service roundtrip");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
    assert_eq!(
        std::fs::read(dir.path().join("target/veryl-components/out/t/state.bin")).unwrap(),
        42u64.to_le_bytes()
    );
}

#[test]
fn upstream_expression_hoisting_forms_preserve_call_order() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_counter;
            var sum: logic<16>;

            initial {
                component.set(41);
                $assert(component.get() == 41, "call inside assert");
                if component.get() == 41 {
                    component.set(component.get() + 1);
                } else {
                    component.set(0);
                }
                let direct: logic<8> = component.get();
                let arithmetic: logic<8> = component.get() + 1;
                $assert(direct == 42, "bare call let initializer");
                $assert(arithmetic == 43, "expression let initializer");

                component.set(0);
                sum = 0;
                for i in 0..5 {
                    let value: logic<16> = component.bump() + 0;
                    sum += value;
                }
                $assert(sum == 15, "hoisted call re-executes in every iteration");
                $assert(component.get() == 5, "five bumps retained state");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .opt_level(celox::OptLevel::O2)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );

    let (_dir, metadata) = component_metadata();
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .opt_level(celox::OptLevel::O2)
            .run_test_cranelift()
            .unwrap(),
        TestResult::Pass
    );

    let (_dir, metadata) = component_metadata();
    let mut simulator = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .opt_level(celox::OptLevel::O2)
        .build_wasm()
        .unwrap();
    let testbench = celox::testbench::compile_initial_testbench(&simulator).unwrap();
    assert_eq!(
        celox::testbench::run_compiled_testbench(&mut simulator, &testbench),
        TestResult::Pass
    );
}

#[test]
fn upstream_direct_dynamic_wide_return_truncates_like_assignment() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            var value: logic<64>;
            initial {
                value = component.wide();
                $assert(value == 64'd1, "direct assignment keeps the low word");
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn upstream_var_generic_parameter_resolves_declared_return_width() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            const WIDTH: u32 = 96;
            var component: $comp::celox_param::<WIDTH>;
            var value: logic<96>;
            initial {
                value = component.wide_declared();
                $assert(value == 96'h0000_0002_0000_0000_0000_0001);
                $assert(component.wide_declared() == value);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn component_return_destination_is_modeled_as_a_dynamic_loop_write() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_counter;
            var limit: logic<8>;
            initial {
                limit = 3;
                for i in 0..limit {
                    limit = component.get();
                }
                $finish();
            }
        }
    "#;

    let error = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .build()
        .expect_err("component return must be treated as a loop-body write");
    assert!(matches!(
        error.kind(),
        SimulatorErrorKind::Frontend(diagnostics)
            if diagnostics.iter().any(|diagnostic| matches!(
                diagnostic,
                FrontendDiagnostic::MutableForBound { .. }
            ))
    ));
}

#[test]
fn declared_wide_return_roundtrips() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_counter;
            var value: logic<96>;
            initial {
                value = component.wide_declared();
                $assert(value == 96'h0000_0002_0000_0000_0000_0001);
                $finish();
            }
        }
    "#;

    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}

#[test]
fn missing_component_return_value_is_reported() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            var value: logic<8>;
            initial {
                value = component.unit();
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected missing component return failure");
    };
    assert!(
        message.contains("returned no bit value"),
        "unexpected failure: {message}"
    );
}

#[test]
fn incompatible_component_return_type_is_reported() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            var value: logic<8>;
            initial {
                value = component.string_return();
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected incompatible component return failure");
    };
    assert!(message.contains("returned no bit value"), "{message}");
}

#[test]
fn missing_component_type_fails_during_testbench_initialization() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_missing;
            initial {
                component.noop();
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected missing component failure");
    };
    assert!(
        message.contains("component type `celox_missing` not found"),
        "unexpected failure: {message}"
    );
}

#[test]
fn component_dispatch_failure_is_reported() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            initial {
                component.no_such_method();
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected method dispatch failure");
    };
    assert!(
        message.contains("component method `component.no_such_method` failed"),
        "unexpected failure: {message}"
    );
}

#[test]
fn component_reported_failure_keeps_instance_context() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            initial {
                component.report_fail();
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected component-reported failure");
    };
    assert!(message.contains("[component]"), "{message}");
    assert!(message.contains("reported failure"), "{message}");
}

#[test]
fn strict_expression_temporary_rejects_wide_undeclared_return() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            var value: logic<64>;
            initial {
                value = component.wide() + 0;
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected strict temporary width failure");
    };
    assert!(
        message.contains("expression form carries at most 64 bits"),
        "unexpected failure: {message}"
    );
}

#[test]
fn component_declared_return_width_is_enforced() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_counter;
            var value: logic<8>;

            initial {
                component.set(1);
                value = component.lying();
                $finish();
            }
        }
    "#;

    let TestResult::Fail(message) = Simulator::builder(code, "t")
        .with_metadata(metadata)
        .run_test()
        .unwrap()
    else {
        panic!("expected component return-width failure");
    };
    assert!(
        message.contains("declares a 8-bit return value but returned 16 bits"),
        "unexpected failure: {message}"
    );
}

#[test]
fn component_return_supports_indexed_destinations() {
    register_component();
    let (_dir, metadata) = component_metadata();
    let code = r#"
        #[test(t)]
        module t {
            var component: $comp::celox_unchecked;
            var values: logic<8> [4];
            initial {
                for i in 0..4 {
                    values[i] = 0;
                }
                component.set(42);
                values[1] = component.get();
                $assert(values[1] == 42, "indexed destination");
                $assert(values[0] == 0, "untouched element");
                $finish();
            }
        }
    "#;
    assert_eq!(
        Simulator::builder(code, "t")
            .with_metadata(metadata)
            .run_test()
            .unwrap(),
        TestResult::Pass
    );
}
