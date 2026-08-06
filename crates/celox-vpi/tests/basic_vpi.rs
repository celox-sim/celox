use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicI32, AtomicUsize, Ordering},
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
    ) {
        inst child: Child (
            i: a,
            o: y,
        );
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
            CALLBACKS_SEEN.fetch_or(2, Ordering::SeqCst);
            vpi_control(VPI_FINISH);
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
            c"Top.child[0]"
        );
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
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(
        NativeProgramInstance::from_image(simulator.shared_code().program_image().clone()).unwrap(),
    );

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
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
    }
    clear_runtime();
}

static CALLBACK_VALUE: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" fn record_changed_value(data: *mut VpiCbData) -> i32 {
    // Safety: callback storage and its requested value are live while firing.
    let value = unsafe { (*(*data).value).value.integer };
    CALLBACK_VALUE.store(value, Ordering::SeqCst);
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
