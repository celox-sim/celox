use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};

use celox::{NativeProgramInstance, Simulator};
use celox_vpi::*;

const DESIGN: &str = r#"
    module Child (
        i: input logic<8>,
        o: output logic<8>,
    ) {
        assign o = i + 1;
    }

    module Top (
        a: input logic<8>,
        y: output logic<8>,
        z: output logic<8>,
    ) {
        inst child: Child (
            i: a,
            o: y,
        );
        assign z = y + 1;
    }
"#;

const TWO_CLOCKS: &str = r#"
    module Top (
        clk_a: input  'a clock,
        clk_b: input  'b clock,
        rst_a: input  'a reset,
        rst_b: input  'b reset,
        qa:    output 'a logic<8>,
        qb:    output 'b logic<8>,
    ) {
        var ra: 'a logic<8>;
        var rb: 'b logic<8>;

        unsafe (cdc) {
            always_ff (clk_a, rst_a) {
                if_reset {
                    ra = 0;
                } else {
                    ra = rb + 1;
                }
            }
            always_ff (clk_b, rst_b) {
                if_reset {
                    rb = 0;
                } else {
                    rb = ra + 1;
                }
            }
        }
        assign qa = ra;
        assign qb = rb;
    }
"#;

const COUNTER: &str = r#"
    module Top (
        clk: input clock,
        q: output logic<8>,
    ) {
        always_ff (clk) {
            q += 1;
        }
    }
"#;

const EDGE_SAMPLER: &str = r#"
    module Top (
        clk: input clock,
        d: input logic<8>,
        newest: output logic<8>,
        previous: output logic<8>,
    ) {
        always_ff (clk) {
            newest = d;
            previous = newest;
        }
    }
"#;

const BRANCHED_COMB: &str = r#"
    module Top (
        sel: input logic,
        a: input logic<8>,
        b: input logic<8>,
        y: output logic<8>,
        z: output logic<8>,
    ) {
        always_comb {
            if sel {
                y = a;
            } else {
                y = b;
            }
            z = y + 1;
        }
    }
"#;

const ALIASED_CLOCK: &str = r#"
    module Child (
        clk: input clock,
        q: output logic<8>,
    ) {
        always_ff (clk) {
            q += 1;
        }
    }

    module Top (
        clk: input clock,
        q: output logic<8>,
    ) {
        inst child: Child (
            clk: clk,
            q: q,
        );
    }
"#;

const WIDE_FOUR_STATE: &str = r#"
    module Top (
        signed_in: input signed logic<64>,
        narrow_signed_in: input signed logic<8>,
        four_in: input logic<4>,
        signed_out: output signed logic<64>,
        narrow_signed_out: output signed logic<8>,
        four_out: output logic<4>,
    ) {
        assign signed_out = signed_in;
        assign narrow_signed_out = narrow_signed_in;
        assign four_out = four_in;
    }
"#;

const FATAL_ASSERT: &str = r#"
    module Top (
        a: input logic<8>,
    ) {
        always_comb {
            $assert(a != 8'd1, "fatal a=%0d", a);
        }
    }
"#;

#[test]
fn value_layout_matches_vpi_user_header() {
    assert_eq!(std::mem::size_of::<VpiVecVal>(), 8);
    assert_eq!(std::mem::align_of::<VpiVecVal>(), 4);
    assert_eq!(std::mem::offset_of!(VpiValue, format), 0);
    assert_eq!(std::mem::offset_of!(VpiValue, value), 8);
    assert_eq!(std::mem::size_of::<VpiValue>(), 16);
}

static CALLBACKS_SEEN: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn record_callback(data: *mut VpiCbData) -> i32 {
    // Safety: the callback runtime passes back the registration storage.
    let reason = unsafe { (*data).reason };
    match reason {
        CB_START_OF_SIMULATION => {
            CALLBACKS_SEEN.fetch_or(1, Ordering::SeqCst);
        }
        CB_AFTER_DELAY => {
            assert_eq!(celox_vpi_current_time(), 5);
            let mut scaled = VpiTime {
                type_: VPI_SCALED_REAL_TIME,
                high: u32::MAX,
                low: u32::MAX,
                real: -1.0,
            };
            // Safety: `scaled` is live writable storage for the duration of the call.
            unsafe { vpi_get_time(ptr::null_mut(), &mut scaled) };
            assert_eq!(scaled.high, 0);
            assert_eq!(scaled.low, 0);
            assert_eq!(scaled.real, 5.0);

            let mut integer = VpiTime {
                type_: VPI_SIM_TIME,
                high: u32::MAX,
                low: u32::MAX,
                real: -1.0,
            };
            // Safety: `integer` is live writable storage for the duration of the call.
            unsafe { vpi_get_time(ptr::null_mut(), &mut integer) };
            assert_eq!(integer.high, 0);
            assert_eq!(integer.low, 5);
            assert_eq!(integer.real, 0.0);
            CALLBACKS_SEEN.fetch_or(2, Ordering::SeqCst);
            assert_eq!(vpi_control(VPI_FINISH), 1);
        }
        CB_END_OF_SIMULATION => {
            CALLBACKS_SEEN.fetch_or(4, Ordering::SeqCst);
        }
        _ => unreachable!(),
    }
    0
}

#[test]
fn callback_runtime_advances_time_and_finishes_regions() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let runtime =
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap();
    install_runtime(runtime);
    CALLBACKS_SEEN.store(0, Ordering::SeqCst);

    let mut start_time = VpiTime::default();
    let mut delay_time = VpiTime {
        type_: 2,
        high: 0,
        low: 5,
        real: 0.0,
    };
    let mut end_time = VpiTime::default();
    let start = VpiCbData {
        reason: CB_START_OF_SIMULATION,
        cb_rtn: Some(record_callback),
        obj: ptr::null_mut(),
        time: &mut start_time,
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    let delayed = VpiCbData {
        reason: CB_AFTER_DELAY,
        time: &mut delay_time,
        ..start
    };
    let end = VpiCbData {
        reason: CB_END_OF_SIMULATION,
        time: &mut end_time,
        ..start
    };
    unsafe {
        assert!(!vpi_register_cb(&start).is_null());
        assert!(!vpi_register_cb(&delayed).is_null());
        assert!(!vpi_register_cb(&end).is_null());
    }
    assert!(run_callbacks());
    assert_eq!(CALLBACKS_SEEN.load(Ordering::SeqCst), 7);
    clear_runtime();
}

#[test]
fn vpi_discovers_hierarchy_and_reads_and_writes_values() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let container = simulator
        .shared_code()
        .program_image()
        .to_container_bytes()
        .unwrap();
    let runtime = NativeProgramInstance::from_attached_bytes(&container).unwrap();
    drop(simulator);
    install_runtime(runtime);

    unsafe {
        let top = vpi_handle_by_name(c"Top".as_ptr(), ptr::null_mut());
        assert!(!top.is_null());
        assert_eq!(vpi_get(VPI_TYPE, top), VPI_MODULE);
        assert_eq!(vpi_get(VPI_TOP_MODULE, top), 1);
        assert_eq!(CStr::from_ptr(vpi_get_str(VPI_NAME, top)), c"Top");

        let modules = vpi_iterate(VPI_MODULE, top);
        let child = vpi_scan(modules);
        assert!(!child.is_null());
        assert_eq!(
            CStr::from_ptr(vpi_get_str(VPI_FULL_NAME, child)),
            c"Top.child"
        );
        assert!(!vpi_handle_by_name(c"Top.child".as_ptr(), ptr::null_mut()).is_null());
        assert!(vpi_scan(modules).is_null());
        assert_eq!(vpi_free_object(modules), 1);

        let a = vpi_handle_by_name(c"a".as_ptr(), top);
        let y = vpi_handle_by_name(c"y".as_ptr(), top);
        assert_eq!(vpi_get(VPI_TYPE, a), VPI_REG);
        assert_eq!(vpi_get(VPI_DIRECTION, a), VPI_INPUT);
        assert_eq!(vpi_get(VPI_SIZE, a), 8);
        let parent = vpi_handle(VPI_SCOPE, a);
        assert!(!parent.is_null());

        let input = VpiValue {
            format: VPI_BIN_STR_VAL,
            value: VpiValueData {
                str_: c"00101001".as_ptr().cast_mut(),
            },
        };
        assert_eq!(vpi_put_value(a, &input, ptr::null(), VPI_NO_DELAY), a);

        let mut output = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(y, &mut output);
        assert_eq!(output.value.integer, 42);

        output.format = VPI_BIN_STR_VAL;
        vpi_get_value(y, &mut output);
        assert_eq!(CStr::from_ptr(output.value.str_), c"00101010");

        output.format = VPI_HEX_STR_VAL;
        vpi_get_value(y, &mut output);
        assert_eq!(CStr::from_ptr(output.value.str_), c"2a");

        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
        assert_eq!(vpi_free_object(parent), 1);
        assert_eq!(vpi_free_object(child), 1);
        assert_eq!(vpi_free_object(top), 1);
    }
    clear_runtime();
}

#[test]
fn string_separators_delay_flags_and_force_release_follow_vpi_semantics() {
    let simulator = Simulator::builder(DESIGN, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let z = vpi_handle_by_name(c"Top.z".as_ptr(), ptr::null_mut());
        let separated = VpiValue {
            format: VPI_BIN_STR_VAL,
            value: VpiValueData {
                str_: c"0010_1001".as_ptr().cast_mut(),
            },
        };
        assert_eq!(vpi_put_value(a, &separated, ptr::null(), VPI_NO_DELAY), a);

        let mut read = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(y, &mut read);
        assert_eq!(read.value.integer, 42);

        let rejected = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 10 },
        };
        let delayed = VpiTime {
            type_: 2,
            high: 0,
            low: 1,
            real: 0.0,
        };
        assert!(
            vpi_put_value(
                a,
                &rejected,
                (&raw const delayed).cast(),
                VPI_INERTIAL_DELAY,
            )
            .is_null()
        );
        vpi_get_value(y, &mut read);
        assert_eq!(read.value.integer, 42);

        assert_eq!(
            vpi_put_value(a, &rejected, ptr::null(), VPI_INERTIAL_DELAY),
            a
        );
        vpi_get_value(y, &mut read);
        assert_eq!(read.value.integer, 11);

        let forced_input = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 5 },
        };
        assert_eq!(
            vpi_put_value(a, &forced_input, ptr::null(), VPI_FORCE_FLAG),
            a
        );
        put_int(a, 10);
        vpi_get_value(y, &mut read);
        assert_eq!(read.value.integer, 6);
        assert_eq!(
            vpi_put_value(a, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
            a
        );
        vpi_get_value(y, &mut read);
        assert_eq!(read.value.integer, 11);

        let forced = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 99 },
        };
        assert_eq!(vpi_put_value(y, &forced, ptr::null(), VPI_FORCE_FLAG), y);
        vpi_get_value(y, &mut read);
        assert_eq!(read.value.integer, 99);

        assert_eq!(vpi_put_value(a, &rejected, ptr::null(), VPI_NO_DELAY), a);
        vpi_get_value(y, &mut read);
        assert_eq!(
            read.value.integer, 99,
            "design writes must not override force"
        );
        vpi_get_value(z, &mut read);
        assert_eq!(read.value.integer, 100, "RTL must observe the forced value");

        assert_eq!(
            vpi_put_value(y, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
            y
        );
        vpi_get_value(y, &mut read);
        assert_eq!(
            read.value.integer, 11,
            "release must restore driver control"
        );
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
        assert_eq!(vpi_free_object(z), 1);
    }
    clear_runtime();
}

#[test]
fn force_is_reapplied_between_stores_in_branched_comb_logic() {
    let simulator = Simulator::builder(BRANCHED_COMB, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let sel = vpi_handle_by_name(c"Top.sel".as_ptr(), ptr::null_mut());
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let b = vpi_handle_by_name(c"Top.b".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let z = vpi_handle_by_name(c"Top.z".as_ptr(), ptr::null_mut());
        put_int(sel, 1);
        put_int(a, 10);
        put_int(b, 20);

        let forced = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 99 },
        };
        assert_eq!(vpi_put_value(y, &forced, ptr::null(), VPI_FORCE_FLAG), y);
        put_int(a, 11);

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(z, &mut value);
        assert_eq!(value.value.integer, 100);

        for handle in [sel, a, b, y, z] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

#[test]
fn force_state_is_shared_by_reflected_clock_aliases() {
    let simulator = Simulator::builder(ALIASED_CLOCK, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let top_clock = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let child_clock = vpi_handle_by_name(c"Top.child.clk".as_ptr(), ptr::null_mut());
        let q = vpi_handle_by_name(c"Top.q".as_ptr(), ptr::null_mut());
        assert!(!top_clock.is_null());
        assert!(!child_clock.is_null());
        assert!(!q.is_null());
        put_int(top_clock, 0);
        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        vpi_get_value(q, &mut value);
        let q_before_force = value.value.integer;

        let low = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        assert_eq!(
            vpi_put_value(top_clock, &low, ptr::null(), VPI_FORCE_FLAG),
            top_clock
        );
        vpi_get_value(q, &mut value);
        assert_eq!(
            value.value.integer, q_before_force,
            "forcing low must not create an edge"
        );
        put_int(child_clock, 1);

        vpi_get_value(child_clock, &mut value);
        assert_eq!(value.value.integer, 0, "alias reads must observe the force");
        vpi_get_value(q, &mut value);
        assert_eq!(
            value.value.integer, q_before_force,
            "an alias deposit must not create an edge while forced"
        );

        assert_eq!(
            vpi_put_value(child_clock, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
            child_clock
        );
        vpi_get_value(q, &mut value);
        assert_eq!(
            value.value.integer,
            q_before_force + 1,
            "release through an alias must restore the shared deposited value"
        );

        for handle in [top_clock, child_clock, q] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

static CALLBACK_VALUE: AtomicI32 = AtomicI32::new(-1);
static VALUE_CHANGE_DRIVE: AtomicBool = AtomicBool::new(false);
static VALUE_CHANGE_INPUT: AtomicUsize = AtomicUsize::new(0);
static READ_ONLY_VALUE: AtomicI32 = AtomicI32::new(-1);
static SELF_CHANGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SELF_CHANGE_VALUE: AtomicI32 = AtomicI32::new(-1);
static REGION_ORDER: AtomicUsize = AtomicUsize::new(0);
static FATAL_PHASES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn record_changed_value(data: *mut VpiCbData) -> i32 {
    // Safety: callback storage and its requested value are live while firing.
    let value = unsafe { (*(*data).value).value.integer };
    CALLBACK_VALUE.store(value, Ordering::SeqCst);
    0
}

unsafe extern "C" fn record_read_only_value(data: *mut VpiCbData) -> i32 {
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    // Safety: the callback object remains live throughout the callback.
    unsafe { vpi_get_value((*data).obj, &mut value) };
    READ_ONLY_VALUE.store(unsafe { value.value.integer }, Ordering::SeqCst);
    0
}

unsafe extern "C" fn drive_then_request_read_only(data: *mut VpiCbData) -> i32 {
    if !VALUE_CHANGE_DRIVE.swap(false, Ordering::SeqCst) {
        return 0;
    }
    let input = VALUE_CHANGE_INPUT.load(Ordering::SeqCst) as VpiHandle;
    unsafe { put_int(input, 41) };
    let read_only = VpiCbData {
        reason: CB_READ_ONLY_SYNCH,
        cb_rtn: Some(record_read_only_value),
        obj: unsafe { (*data).obj },
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    assert!(!unsafe { vpi_register_cb(&read_only) }.is_null());
    0
}

unsafe extern "C" fn change_watched_signal_once(data: *mut VpiCbData) -> i32 {
    let value = unsafe { (*(*data).value).value.integer };
    SELF_CHANGE_VALUE.store(value, Ordering::SeqCst);
    let invocation = SELF_CHANGE_COUNT.fetch_add(1, Ordering::SeqCst);
    if invocation == 0 {
        unsafe { put_int((*data).obj, 2) };
    }
    0
}

fn record_region(digit: usize) {
    REGION_ORDER
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |order| {
            Some(order * 10 + digit)
        })
        .unwrap();
}

unsafe extern "C" fn record_read_write_region(_data: *mut VpiCbData) -> i32 {
    record_region(2);
    0
}

unsafe extern "C" fn record_read_only_region(_data: *mut VpiCbData) -> i32 {
    record_region(3);
    0
}

unsafe extern "C" fn record_value_change_region(_data: *mut VpiCbData) -> i32 {
    record_region(1);
    let read_write = VpiCbData {
        reason: CB_READ_WRITE_SYNCH,
        cb_rtn: Some(record_read_write_region),
        obj: ptr::null_mut(),
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    assert!(!unsafe { vpi_register_cb(&read_write) }.is_null());
    0
}

unsafe extern "C" fn record_fatal_phase(data: *mut VpiCbData) -> i32 {
    // Safety: callback storage remains live while the runtime invokes it.
    let reason = unsafe { (*data).reason };
    let bit = match reason {
        CB_VALUE_CHANGE => 1,
        CB_READ_WRITE_SYNCH => 2,
        CB_READ_ONLY_SYNCH => 4,
        _ => unreachable!(),
    };
    FATAL_PHASES.fetch_or(bit, Ordering::SeqCst);
    0
}

unsafe extern "C" fn trigger_fatal_and_register_phases(data: *mut VpiCbData) -> i32 {
    // Safety: the callback object is the live signal handle supplied below.
    let signal = unsafe { (*data).obj };
    let value_change = VpiCbData {
        reason: CB_VALUE_CHANGE,
        cb_rtn: Some(record_fatal_phase),
        obj: signal,
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    let read_write = VpiCbData {
        reason: CB_READ_WRITE_SYNCH,
        obj: ptr::null_mut(),
        ..value_change
    };
    let read_only = VpiCbData {
        reason: CB_READ_ONLY_SYNCH,
        ..read_write
    };
    assert!(!unsafe { vpi_register_cb(&value_change) }.is_null());
    assert!(!unsafe { vpi_register_cb(&read_write) }.is_null());
    assert!(!unsafe { vpi_register_cb(&read_only) }.is_null());
    unsafe { put_int(signal, 1) };
    0
}

#[test]
fn value_change_callback_receives_the_requested_current_value() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );
    CALLBACK_VALUE.store(-1, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let mut callback_value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        let callback = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(record_changed_value),
            obj: y,
            time: ptr::null_mut(),
            value: &mut callback_value,
            index: 0,
            user_data: ptr::null_mut(),
        };
        let callback_handle = vpi_register_cb(&callback);
        assert!(!callback_handle.is_null());

        let input = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 41 },
        };
        assert_eq!(vpi_put_value(a, &input, ptr::null(), VPI_NO_DELAY), a);
        assert!(!run_callbacks());
        assert_eq!(CALLBACK_VALUE.load(Ordering::SeqCst), 42);

        assert_eq!(vpi_remove_cb(callback_handle), 1);
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    clear_runtime();
}

#[test]
fn value_change_writes_settle_before_read_only_callbacks() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );
    VALUE_CHANGE_DRIVE.store(true, Ordering::SeqCst);
    READ_ONLY_VALUE.store(-1, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        VALUE_CHANGE_INPUT.store(a as usize, Ordering::SeqCst);
        let callback = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(drive_then_request_read_only),
            obj: y,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let callback_handle = vpi_register_cb(&callback);
        assert!(!callback_handle.is_null());
        put_int(a, 1);
        assert!(!run_callbacks());
        assert_eq!(READ_ONLY_VALUE.load(Ordering::SeqCst), 42);
        assert_eq!(vpi_remove_cb(callback_handle), 1);
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    VALUE_CHANGE_INPUT.store(0, Ordering::SeqCst);
    clear_runtime();
}

#[test]
fn value_change_callback_observes_a_change_it_makes_itself() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );
    SELF_CHANGE_COUNT.store(0, Ordering::SeqCst);
    SELF_CHANGE_VALUE.store(-1, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let mut callback_value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        let callback = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(change_watched_signal_once),
            obj: a,
            time: ptr::null_mut(),
            value: &mut callback_value,
            index: 0,
            user_data: ptr::null_mut(),
        };
        let callback_handle = vpi_register_cb(&callback);
        assert!(!callback_handle.is_null());
        put_int(a, 1);
        assert!(!run_callbacks());
        assert_eq!(SELF_CHANGE_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(SELF_CHANGE_VALUE.load(Ordering::SeqCst), 2);
        assert_eq!(vpi_remove_cb(callback_handle), 1);
        assert_eq!(vpi_free_object(a), 1);
    }
    clear_runtime();
}

#[test]
fn value_change_precedes_read_write_and_read_only_regions() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );
    REGION_ORDER.store(0, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let value_change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(record_value_change_region),
            obj: y,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let read_only = VpiCbData {
            reason: CB_READ_ONLY_SYNCH,
            cb_rtn: Some(record_read_only_region),
            obj: ptr::null_mut(),
            ..value_change
        };
        let value_change_handle = vpi_register_cb(&value_change);
        assert!(!value_change_handle.is_null());
        assert!(!vpi_register_cb(&read_only).is_null());
        put_int(a, 1);
        assert!(!run_callbacks());
        assert_eq!(REGION_ORDER.load(Ordering::SeqCst), 123);
        assert_eq!(vpi_remove_cb(value_change_handle), 1);
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    clear_runtime();
}

#[test]
fn fatal_settle_stops_remaining_callback_phases() {
    let simulator = Simulator::builder(FATAL_ASSERT, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );
    FATAL_PHASES.store(0, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let mut delay = VpiTime {
            type_: 2,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let delayed = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(trigger_fatal_and_register_phases),
            obj: a,
            time: &mut delay,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&delayed).is_null());

        let error = run_callbacks_result().unwrap_err();
        assert!(error.contains("fatal a=1"));
        assert_eq!(FATAL_PHASES.load(Ordering::SeqCst), 0);
    }
    clear_runtime();
}

#[test]
fn signed_integer_deposits_and_z_values_round_trip() {
    let simulator = Simulator::builder(WIDE_FOUR_STATE, "Top")
        .four_state(true)
        .build()
        .unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let signed_in = vpi_handle_by_name(c"Top.signed_in".as_ptr(), ptr::null_mut());
        let signed_out = vpi_handle_by_name(c"Top.signed_out".as_ptr(), ptr::null_mut());
        let narrow_signed_in =
            vpi_handle_by_name(c"Top.narrow_signed_in".as_ptr(), ptr::null_mut());
        let narrow_signed_out =
            vpi_handle_by_name(c"Top.narrow_signed_out".as_ptr(), ptr::null_mut());
        let four_in = vpi_handle_by_name(c"Top.four_in".as_ptr(), ptr::null_mut());
        let four_out = vpi_handle_by_name(c"Top.four_out".as_ptr(), ptr::null_mut());
        put_int(signed_in, -1);
        put_int(narrow_signed_in, -1);

        let mut vector = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData {
                vector: ptr::null_mut(),
            },
        };
        vpi_get_value(signed_out, &mut vector);
        let words = std::slice::from_raw_parts(vector.value.vector, 2);
        assert_eq!(words[0].aval as u32, u32::MAX);
        assert_eq!(words[1].aval as u32, u32::MAX);
        assert_eq!(words[0].bval, 0);
        assert_eq!(words[1].bval, 0);

        let mut integer = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(narrow_signed_out, &mut integer);
        assert_eq!(integer.value.integer, -1);

        let four = VpiValue {
            format: VPI_BIN_STR_VAL,
            value: VpiValueData {
                str_: c"z01x".as_ptr().cast_mut(),
            },
        };
        assert_eq!(
            vpi_put_value(four_in, &four, ptr::null(), VPI_NO_DELAY),
            four_in
        );
        let mut string = VpiValue {
            format: VPI_BIN_STR_VAL,
            value: VpiValueData {
                str_: ptr::null_mut(),
            },
        };
        vpi_get_value(four_out, &mut string);
        assert_eq!(CStr::from_ptr(string.value.str_), c"z01x");

        for handle in [
            signed_in,
            signed_out,
            narrow_signed_in,
            narrow_signed_out,
            four_in,
            four_out,
        ] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

unsafe extern "C" fn drive_callback_object_high(data: *mut VpiCbData) -> i32 {
    let high = VpiValue {
        format: VPI_SCALAR_VAL,
        value: VpiValueData { scalar: VPI_1 },
    };
    // Safety: the callback object is the live clock handle registered below.
    unsafe {
        assert_eq!(
            vpi_put_value((*data).obj, &high, ptr::null(), VPI_NO_DELAY),
            (*data).obj
        );
    }
    0
}

unsafe extern "C" fn drive_two_four_state_posedges(data: *mut VpiCbData) -> i32 {
    let unknown = VpiValue {
        format: VPI_SCALAR_VAL,
        value: VpiValueData { scalar: VPI_X },
    };
    let high = VpiValue {
        format: VPI_SCALAR_VAL,
        value: VpiValueData { scalar: VPI_1 },
    };
    // Safety: the callback object is the live clock handle registered below.
    unsafe {
        assert_eq!(
            vpi_put_value((*data).obj, &unknown, ptr::null(), VPI_NO_DELAY),
            (*data).obj
        );
        assert_eq!(
            vpi_put_value((*data).obj, &high, ptr::null(), VPI_NO_DELAY),
            (*data).obj
        );
    }
    0
}

static EDGE_DATA_HANDLE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn drive_two_edges_with_distinct_data(data: *mut VpiCbData) -> i32 {
    let clk = unsafe { (*data).obj };
    let d = EDGE_DATA_HANDLE.load(Ordering::SeqCst) as VpiHandle;
    unsafe {
        put_int(clk, 1);
        put_int(d, 1);
        put_int(clk, 0);
        put_int(d, 2);
        put_int(clk, 1);
    }
    0
}

unsafe fn put_int(handle: VpiHandle, integer: i32) {
    let value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer },
    };
    // Safety: forwarded from the caller's live handle.
    assert_eq!(
        unsafe { vpi_put_value(handle, &value, ptr::null(), VPI_NO_DELAY) },
        handle
    );
}

#[test]
fn same_time_clock_callbacks_commit_domains_simultaneously() {
    let simulator = Simulator::builder(TWO_CLOCKS, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let clk_a = vpi_handle_by_name(c"Top.clk_a".as_ptr(), ptr::null_mut());
        let clk_b = vpi_handle_by_name(c"Top.clk_b".as_ptr(), ptr::null_mut());
        let rst_a = vpi_handle_by_name(c"Top.rst_a".as_ptr(), ptr::null_mut());
        let rst_b = vpi_handle_by_name(c"Top.rst_b".as_ptr(), ptr::null_mut());
        let qa = vpi_handle_by_name(c"Top.qa".as_ptr(), ptr::null_mut());
        let qb = vpi_handle_by_name(c"Top.qb".as_ptr(), ptr::null_mut());

        put_int(clk_a, 0);
        put_int(clk_b, 0);
        put_int(rst_a, 0);
        put_int(rst_b, 0);
        put_int(clk_a, 1);
        put_int(clk_a, 0);
        put_int(clk_b, 1);
        put_int(clk_b, 0);
        put_int(rst_a, 1);
        put_int(rst_b, 1);

        let mut time_a = VpiTime {
            type_: 2,
            high: 0,
            low: 5,
            real: 0.0,
        };
        let mut time_b = time_a;
        let callback_a = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_callback_object_high),
            obj: clk_a,
            time: &mut time_a,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let callback_b = VpiCbData {
            obj: clk_b,
            time: &mut time_b,
            ..callback_a
        };
        assert!(!vpi_register_cb(&callback_a).is_null());
        assert!(!vpi_register_cb(&callback_b).is_null());
        assert!(!run_callbacks());

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(qa, &mut value);
        assert_eq!(value.value.integer, 1);
        vpi_get_value(qb, &mut value);
        assert_eq!(value.value.integer, 1);

        for handle in [clk_a, clk_b, rst_a, rst_b, qa, qb] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

#[test]
fn repeated_four_state_clock_edges_in_one_callback_are_not_lost() {
    let simulator = Simulator::builder(COUNTER, "Top")
        .four_state(true)
        .build()
        .unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let q = vpi_handle_by_name(c"Top.q".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(q, 0);
        let mut time = VpiTime {
            type_: 2,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let callback = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_two_four_state_posedges),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&callback).is_null());
        assert!(!run_callbacks());

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(q, &mut value);
        assert_eq!(value.value.integer, 2);
        assert_eq!(vpi_free_object(clk), 1);
        assert_eq!(vpi_free_object(q), 1);
    }
    clear_runtime();
}

#[test]
fn repeated_clock_edges_preserve_the_data_at_each_edge() {
    let simulator = Simulator::builder(EDGE_SAMPLER, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let d = vpi_handle_by_name(c"Top.d".as_ptr(), ptr::null_mut());
        let newest = vpi_handle_by_name(c"Top.newest".as_ptr(), ptr::null_mut());
        let previous = vpi_handle_by_name(c"Top.previous".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(d, 0);
        put_int(newest, 0);
        put_int(previous, 0);
        EDGE_DATA_HANDLE.store(d as usize, Ordering::SeqCst);

        let mut time = VpiTime {
            type_: 2,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let callback = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_two_edges_with_distinct_data),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&callback).is_null());
        assert!(!run_callbacks());

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(newest, &mut value);
        assert_eq!(value.value.integer, 2);
        vpi_get_value(previous, &mut value);
        assert_eq!(value.value.integer, 1);

        for handle in [clk, d, newest, previous] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    EDGE_DATA_HANDLE.store(0, Ordering::SeqCst);
    clear_runtime();
}
