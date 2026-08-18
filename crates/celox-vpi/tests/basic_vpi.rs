use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};

use celox::{NativeProgramInstance, Simulator};
use celox_vpi::*;

fn trusted_instance(image: celox::NativeProgramImage) -> NativeProgramInstance {
    // Safety: tests execute only images produced in-process by the Celox compiler.
    unsafe { NativeProgramInstance::from_image(image) }.unwrap()
}

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

const ORDERED_BRANCHED_COMB: &str = r#"
    module Top (
        sel: input logic,
        y: output logic<8>,
    ) {
        always_comb {
            y = 1;
            if sel {
                y = 2;
            }
        }
    }
"#;

const LOOPED_COMB: &str = r#"
    module Top (
        x: output logic<8>,
        total: output logic<8>,
    ) {
        always_comb {
            total = 0;
            for i in 0..4 {
                x = 1;
                total += x;
            }
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
        unsigned_in: input logic<64>,
        narrow_signed_in: input signed logic<8>,
        four_in: input logic<4>,
        signed_out: output signed logic<64>,
        unsigned_out: output logic<64>,
        narrow_signed_out: output signed logic<8>,
        four_out: output logic<4>,
    ) {
        assign signed_out = signed_in;
        assign unsigned_out = unsigned_in;
        assign narrow_signed_out = narrow_signed_in;
        assign four_out = four_in;
    }
"#;

const TWO_STATE_VALUES: &str = r#"
    module Top (
        a: input bit<4>,
        y: output bit<4>,
    ) {
        assign y = a;
    }
"#;

const FATAL_ASSERT: &str = r#"
    module Child (
        a: input logic<8>,
    ) {
        always_comb {
            $assert(a != 8'd1, "fatal a=%0d time=%t scope=%m", a);
        }
    }

    module Top (
        a: input logic<8>,
    ) {
        inst child: Child (
            a: a,
        );
    }
"#;

const FF_FATAL_ASSERT: &str = r#"
    module Top (
        clk: input clock,
        d: input logic<8>,
    ) {
        always_ff (clk) {
            $assert(1'd0, "ff d=%0d time=%t", d);
        }
    }
"#;

const COMPILE_TIME_OBJECTS: &str = r#"
    module Top #(
        param WIDTH: u32 = 8,
    ) (
        y: output logic<WIDTH>,
    ) {
        const BIAS: logic<WIDTH> = 1;
        assign y = BIAS;
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
static CALLBACKS_AFTER_FINISH: AtomicUsize = AtomicUsize::new(0);
static ALIAS_WRITE_FIRST: AtomicUsize = AtomicUsize::new(0);
static ALIAS_WRITE_SECOND: AtomicUsize = AtomicUsize::new(0);
static STARTUP_TIMER_VALUE: AtomicI32 = AtomicI32::new(-1);
static NEXT_TIME_TIMER_VALUE: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" fn drive_before_due_timer(data: *mut VpiCbData) -> i32 {
    let value = match unsafe { (*data).reason } {
        CB_START_OF_SIMULATION => 10,
        CB_NEXT_SIM_TIME => 20,
        _ => unreachable!(),
    };
    unsafe { put_int((*data).obj, value) };
    0
}

unsafe extern "C" fn record_due_timer_output(data: *mut VpiCbData) -> i32 {
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    unsafe { vpi_get_value((*data).obj, &mut value) };
    let value = unsafe { value.value.integer };
    match celox_vpi_current_time() {
        0 => STARTUP_TIMER_VALUE.store(value, Ordering::SeqCst),
        5 => NEXT_TIME_TIMER_VALUE.store(value, Ordering::SeqCst),
        time => panic!("unexpected timer time {time}"),
    }
    0
}

unsafe extern "C" fn record_callback(data: *mut VpiCbData) -> i32 {
    // Safety: the callback runtime passes back the registration storage.
    let reason = unsafe { (*data).reason };
    match reason {
        CB_START_OF_SIMULATION => {
            CALLBACKS_SEEN.fetch_or(1, Ordering::SeqCst);
        }
        CB_AFTER_DELAY => {
            assert_eq!(celox_vpi_current_time(), 5);
            // Safety: this callback was registered with live time storage.
            let delivered = unsafe { &*(*data).time };
            assert_eq!(delivered.type_, VPI_SCALED_REAL_TIME);
            assert_eq!(delivered.high, 0);
            assert_eq!(delivered.low, 0);
            assert_eq!(delivered.real, 5.0);
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

unsafe extern "C" fn finish_immediately(_data: *mut VpiCbData) -> i32 {
    assert_eq!(vpi_control(VPI_FINISH), 1);
    0
}

unsafe extern "C" fn record_callback_after_finish(data: *mut VpiCbData) -> i32 {
    // Safety: the callback runtime passes back the live registration storage.
    let bit = match unsafe { (*data).reason } {
        CB_START_OF_SIMULATION => 1,
        CB_READ_WRITE_SYNCH => 2,
        CB_READ_ONLY_SYNCH => 4,
        CB_END_OF_SIMULATION => 8,
        _ => unreachable!(),
    };
    CALLBACKS_AFTER_FINISH.fetch_or(bit, Ordering::SeqCst);
    0
}

unsafe extern "C" fn write_reflected_aliases(_data: *mut VpiCbData) -> i32 {
    let first = ALIAS_WRITE_FIRST.load(Ordering::SeqCst) as VpiHandle;
    let second = ALIAS_WRITE_SECOND.load(Ordering::SeqCst) as VpiHandle;
    unsafe {
        put_int(first, 1);
        put_int(second, 7);
    }
    0
}

#[test]
fn compile_time_objects_are_not_exposed_as_writable_registers() {
    let simulator = Simulator::builder(COMPILE_TIME_OBJECTS, "Top")
        .build()
        .unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        assert!(vpi_handle_by_name(c"Top.WIDTH".as_ptr(), ptr::null_mut()).is_null());
        assert!(vpi_handle_by_name(c"Top.BIAS".as_ptr(), ptr::null_mut()).is_null());
        let output = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        assert!(!output.is_null());
        assert_eq!(vpi_free_object(output), 1);
    }
    clear_runtime();
}

#[test]
fn callback_runtime_advances_time_and_finishes_regions() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let runtime = trusted_instance(simulator.shared_code().program_image().clone());
    install_runtime(runtime);
    CALLBACKS_SEEN.store(0, Ordering::SeqCst);

    let mut start_time = VpiTime::default();
    let mut delay_time = VpiTime {
        type_: VPI_SCALED_REAL_TIME,
        high: u32::MAX,
        low: u32::MAX,
        real: 5.0,
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
fn pre_time_callback_writes_settle_before_due_timers() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    STARTUP_TIMER_VALUE.store(-1, Ordering::SeqCst);
    NEXT_TIME_TIMER_VALUE.store(-1, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let start = VpiCbData {
            reason: CB_START_OF_SIMULATION,
            cb_rtn: Some(drive_before_due_timer),
            obj: a,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let next_time = VpiCbData {
            reason: CB_NEXT_SIM_TIME,
            ..start
        };
        let mut zero = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 0,
            real: 0.0,
        };
        let timer_zero = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(record_due_timer_output),
            obj: y,
            time: &mut zero,
            ..start
        };
        let mut five = VpiTime { low: 5, ..zero };
        let timer_five = VpiCbData {
            time: &mut five,
            ..timer_zero
        };
        for callback in [&start, &next_time, &timer_zero, &timer_five] {
            assert!(!vpi_register_cb(callback).is_null());
        }

        assert!(!run_callbacks());
        assert_eq!(STARTUP_TIMER_VALUE.load(Ordering::SeqCst), 11);
        assert_eq!(NEXT_TIME_TIMER_VALUE.load(Ordering::SeqCst), 21);
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    clear_runtime();
}

#[test]
fn scaled_real_zero_delay_inertial_write_is_immediate() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let input = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 41 },
        };
        let delay = VpiTime {
            type_: VPI_SCALED_REAL_TIME,
            high: u32::MAX,
            low: u32::MAX,
            real: 0.0,
        };
        assert_eq!(
            vpi_put_value(a, &input, ptr::from_ref(&delay).cast(), VPI_INERTIAL_DELAY),
            a
        );
        let mut output = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        vpi_get_value(y, &mut output);
        assert_eq!(output.value.integer, 42);
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    clear_runtime();
}

#[test]
fn finish_stops_remaining_callback_regions_but_still_runs_end_callbacks() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    CALLBACKS_AFTER_FINISH.store(0, Ordering::SeqCst);

    let finish = VpiCbData {
        reason: CB_START_OF_SIMULATION,
        cb_rtn: Some(finish_immediately),
        obj: ptr::null_mut(),
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    let skipped_start = VpiCbData {
        cb_rtn: Some(record_callback_after_finish),
        ..finish
    };
    let skipped_read_write = VpiCbData {
        reason: CB_READ_WRITE_SYNCH,
        ..skipped_start
    };
    let skipped_read_only = VpiCbData {
        reason: CB_READ_ONLY_SYNCH,
        ..skipped_start
    };
    let end = VpiCbData {
        reason: CB_END_OF_SIMULATION,
        ..skipped_start
    };
    unsafe {
        assert!(!vpi_register_cb(&finish).is_null());
        assert!(!vpi_register_cb(&skipped_start).is_null());
        assert!(!vpi_register_cb(&skipped_read_write).is_null());
        assert!(!vpi_register_cb(&skipped_read_only).is_null());
        assert!(!vpi_register_cb(&end).is_null());
    }

    assert!(run_callbacks());
    assert_eq!(CALLBACKS_AFTER_FINISH.load(Ordering::SeqCst), 8);
    clear_runtime();
}

#[test]
fn callback_deposits_preserve_order_across_reflected_aliases() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let parent = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let child = vpi_handle_by_name(c"Top.child.i".as_ptr(), ptr::null_mut());
        let output = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        assert!(!parent.is_null());
        assert!(!child.is_null());
        assert!(!output.is_null());
        ALIAS_WRITE_FIRST.store(parent as usize, Ordering::SeqCst);
        ALIAS_WRITE_SECOND.store(child as usize, Ordering::SeqCst);

        let callback = VpiCbData {
            reason: CB_START_OF_SIMULATION,
            cb_rtn: Some(write_reflected_aliases),
            obj: ptr::null_mut(),
            time: ptr::null_mut(),
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
        vpi_get_value(output, &mut value);
        assert_eq!(value.value.integer, 8);

        for handle in [parent, child, output] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    ALIAS_WRITE_FIRST.store(0, Ordering::SeqCst);
    ALIAS_WRITE_SECOND.store(0, Ordering::SeqCst);
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
    // Safety: the container was created above from an in-process compiled image.
    let runtime = unsafe { NativeProgramInstance::from_attached_bytes(&container) }.unwrap();
    drop(simulator);
    install_runtime(runtime);

    unsafe {
        let top = vpi_handle_by_name(c"Top".as_ptr(), ptr::null_mut());
        assert!(!top.is_null());
        assert_eq!(vpi_get(VPI_TYPE, top), VPI_MODULE);
        assert_eq!(vpi_get(VPI_TOP_MODULE, top), 1);
        assert_eq!(vpi_get(VPI_TIME_UNIT, top), -12);
        assert_eq!(vpi_get(VPI_TIME_PRECISION, top), -12);
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

        let a = vpi_handle_by_name(c"a".as_ptr(), top);
        let y = vpi_handle_by_name(c"y".as_ptr(), top);
        assert_eq!(vpi_get(VPI_TYPE, a), VPI_REG);
        assert_eq!(vpi_get(VPI_DIRECTION, a), VPI_INPUT);
        assert_eq!(vpi_get(VPI_SIZE, a), 8);
        assert_eq!(vpi_get(VPI_SIGNED, a), 0);
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

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
fn rejected_force_does_not_leave_vpi_overlay_or_pending_state() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let forced = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 5 },
        };
        assert!(
            vpi_put_value(a, &forced, ptr::null(), VPI_FORCE_FLAG).is_null(),
            "ordinary images must reject force"
        );

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        vpi_get_value(a, &mut value);
        assert_eq!(value.value.integer, 0);
        vpi_get_value(y, &mut value);
        assert_eq!(value.value.integer, 1);
        assert!(!run_callbacks());
        vpi_get_value(a, &mut value);
        assert_eq!(value.value.integer, 0);

        put_int(a, 10);
        vpi_get_value(y, &mut value);
        assert_eq!(value.value.integer, 11);
        for handle in [a, y] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

#[test]
fn two_state_deposits_and_forces_convert_unknown_bits_to_zero() {
    let simulator = Simulator::builder(TWO_STATE_VALUES, "Top")
        .four_state(true)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let mut word = VpiVecVal {
            aval: 0b1111,
            bval: 0b1010,
        };
        let mut value = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData { vector: &mut word },
        };
        assert_eq!(vpi_put_value(a, &value, ptr::null(), VPI_NO_DELAY), a);
        vpi_get_value(y, &mut value);
        assert_eq!((*value.value.vector).aval, 0b0101);
        assert_eq!((*value.value.vector).bval, 0);

        word = VpiVecVal {
            aval: 0b1111,
            bval: 0b1100,
        };
        value.value.vector = &mut word;
        assert_eq!(vpi_put_value(a, &value, ptr::null(), VPI_FORCE_FLAG), a);
        vpi_get_value(a, &mut value);
        assert_eq!((*value.value.vector).aval, 0b0011);
        assert_eq!((*value.value.vector).bval, 0);

        assert_eq!(
            vpi_put_value(a, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
            a
        );
        vpi_get_value(y, &mut value);
        assert_eq!((*value.value.vector).aval, 0b0101);
        assert_eq!((*value.value.vector).bval, 0);
        for handle in [a, y] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

#[test]
fn releasing_a_sequential_variable_keeps_its_forced_value_until_the_next_edge() {
    let simulator = Simulator::builder(COUNTER, "Top")
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let q = vpi_handle_by_name(c"Top.q".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(q, 0);

        let forced = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 1 },
        };
        assert_eq!(vpi_put_value(q, &forced, ptr::null(), VPI_FORCE_FLAG), q);
        assert_eq!(
            vpi_put_value(q, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
            q
        );

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        vpi_get_value(q, &mut value);
        assert_eq!(value.value.integer, 1);

        put_int(clk, 1);
        vpi_get_value(q, &mut value);
        assert_eq!(value.value.integer, 2);

        for handle in [clk, q] {
            assert_eq!(vpi_free_object(handle), 1);
        }
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

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
fn force_split_preserves_procedural_order_across_branches() {
    let simulator = Simulator::builder(ORDERED_BRANCHED_COMB, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let sel = vpi_handle_by_name(c"Top.sel".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let forced = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 1 },
        };
        assert_eq!(
            vpi_put_value(sel, &forced, ptr::null(), VPI_FORCE_FLAG),
            sel
        );

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(y, &mut value);
        assert_eq!(value.value.integer, 2);

        assert_eq!(vpi_free_object(sel), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    clear_runtime();
}

#[test]
fn force_is_reapplied_between_unrolled_loop_iterations() {
    let simulator = Simulator::builder(LOOPED_COMB, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let x = vpi_handle_by_name(c"Top.x".as_ptr(), ptr::null_mut());
        let total = vpi_handle_by_name(c"Top.total".as_ptr(), ptr::null_mut());
        let forced = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 3 },
        };
        assert_eq!(vpi_put_value(x, &forced, ptr::null(), VPI_FORCE_FLAG), x);

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(total, &mut value);
        assert_eq!(value.value.integer, 12);

        assert_eq!(vpi_free_object(x), 1);
        assert_eq!(vpi_free_object(total), 1);
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

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
static OWNED_CALLBACK_TIME: AtomicUsize = AtomicUsize::new(usize::MAX);
static VALUE_CHANGE_DRIVE: AtomicBool = AtomicBool::new(false);
static VALUE_CHANGE_INPUT: AtomicUsize = AtomicUsize::new(0);
static READ_ONLY_VALUE: AtomicI32 = AtomicI32::new(-1);
static SELF_CHANGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static SELF_CHANGE_VALUE: AtomicI32 = AtomicI32::new(-1);
static BATCH_CALLBACK_RESET: AtomicBool = AtomicBool::new(false);
static BATCH_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static BATCH_CALLBACK_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
static REGION_ORDER: AtomicUsize = AtomicUsize::new(0);
static FATAL_PHASES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" fn record_changed_value(data: *mut VpiCbData) -> i32 {
    // Safety: callback storage and its requested value are live while firing.
    let value = unsafe { (*(*data).value).value.integer };
    CALLBACK_VALUE.store(value, Ordering::SeqCst);
    0
}

unsafe extern "C" fn record_owned_callback_storage(data: *mut VpiCbData) -> i32 {
    let value = unsafe { (*(*data).value).value.integer };
    let time = unsafe { (*(*data).time).low } as usize;
    CALLBACK_VALUE.store(value, Ordering::SeqCst);
    OWNED_CALLBACK_TIME.store(time, Ordering::SeqCst);
    0
}

unsafe fn register_stack_value_callback(object: VpiHandle) -> VpiHandle {
    let mut time = VpiTime {
        type_: VPI_SIM_TIME,
        high: 0,
        low: 0,
        real: 0.0,
    };
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    let callback = VpiCbData {
        reason: CB_VALUE_CHANGE,
        cb_rtn: Some(record_owned_callback_storage),
        obj: object,
        time: &mut time,
        value: &mut value,
        index: 0,
        user_data: ptr::null_mut(),
    };
    unsafe { vpi_register_cb(&callback) }
}

unsafe fn register_stack_timer(object: VpiHandle) {
    let mut time = VpiTime {
        type_: VPI_SIM_TIME,
        high: 0,
        low: 3,
        real: 0.0,
    };
    let callback = VpiCbData {
        reason: CB_AFTER_DELAY,
        cb_rtn: Some(drive_callback_object_high),
        obj: object,
        time: &mut time,
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    assert!(!unsafe { vpi_register_cb(&callback) }.is_null());
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

unsafe extern "C" fn reset_signal_in_first_batch_callback(data: *mut VpiCbData) -> i32 {
    if BATCH_CALLBACK_RESET.swap(false, Ordering::SeqCst) {
        unsafe { put_int((*data).obj, 0) };
    }
    0
}

unsafe extern "C" fn record_batch_callback_value(data: *mut VpiCbData) -> i32 {
    let value = unsafe { (*(*data).value).value.integer } as usize;
    BATCH_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst);
    BATCH_CALLBACK_SEQUENCE
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |sequence| {
            Some(sequence * 10 + value)
        })
        .unwrap();
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
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
fn callback_registration_owns_copied_time_and_value_records() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    CALLBACK_VALUE.store(-1, Ordering::SeqCst);
    OWNED_CALLBACK_TIME.store(usize::MAX, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let value_change = register_stack_value_callback(a);
        assert!(!value_change.is_null());
        register_stack_timer(a);

        // Reuse the caller stack after both registration functions returned.
        let clobber = [0xa5u8; 4096];
        std::hint::black_box(&clobber);
        assert!(!run_callbacks());
        assert_eq!(CALLBACK_VALUE.load(Ordering::SeqCst), 1);
        assert_eq!(OWNED_CALLBACK_TIME.load(Ordering::SeqCst), 3);

        assert_eq!(vpi_remove_cb(value_change), 1);
        assert_eq!(vpi_free_object(a), 1);
    }
    clear_runtime();
}

#[test]
fn value_change_writes_settle_before_read_only_callbacks() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
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
fn callback_batch_preserves_the_value_that_triggered_each_callback() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    BATCH_CALLBACK_RESET.store(true, Ordering::SeqCst);
    BATCH_CALLBACK_COUNT.store(0, Ordering::SeqCst);
    BATCH_CALLBACK_SEQUENCE.store(0, Ordering::SeqCst);

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let reset = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(reset_signal_in_first_batch_callback),
            obj: a,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let reset_handle = vpi_register_cb(&reset);
        assert!(!reset_handle.is_null());

        let mut observed_value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        let observe = VpiCbData {
            cb_rtn: Some(record_batch_callback_value),
            value: &mut observed_value,
            ..reset
        };
        let observe_handle = vpi_register_cb(&observe);
        assert!(!observe_handle.is_null());

        put_int(a, 1);
        assert!(!run_callbacks());
        assert_eq!(BATCH_CALLBACK_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(BATCH_CALLBACK_SEQUENCE.load(Ordering::SeqCst), 10);

        assert_eq!(vpi_remove_cb(reset_handle), 1);
        assert_eq!(vpi_remove_cb(observe_handle), 1);
        assert_eq!(vpi_free_object(a), 1);
    }
    clear_runtime();
}

#[test]
fn value_change_precedes_read_write_and_read_only_regions() {
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
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
        assert!(error.contains("time=1"));
        assert!(error.contains("scope=Top.child"));
        assert_eq!(FATAL_PHASES.load(Ordering::SeqCst), 0);
    }
    clear_runtime();
}

#[test]
fn ff_fatal_error_uses_the_recorded_value_and_scheduler_time() {
    let simulator = Simulator::builder(FF_FATAL_ASSERT, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let mut delay = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 5,
            real: 0.0,
        };
        let rising_edge = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_callback_object_high),
            obj: clk,
            time: &mut delay,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&rising_edge).is_null());

        let error = run_callbacks_result().unwrap_err();
        assert!(error.contains("ff d=0 time=5"), "{error}");
    }
    clear_runtime();
}

#[test]
fn signed_integer_deposits_and_z_values_round_trip() {
    let simulator = Simulator::builder(WIDE_FOUR_STATE, "Top")
        .four_state(true)
        .native_force_support(true)
        .build()
        .unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

    unsafe {
        let signed_in = vpi_handle_by_name(c"Top.signed_in".as_ptr(), ptr::null_mut());
        let signed_out = vpi_handle_by_name(c"Top.signed_out".as_ptr(), ptr::null_mut());
        let unsigned_in = vpi_handle_by_name(c"Top.unsigned_in".as_ptr(), ptr::null_mut());
        let unsigned_out = vpi_handle_by_name(c"Top.unsigned_out".as_ptr(), ptr::null_mut());
        let narrow_signed_in =
            vpi_handle_by_name(c"Top.narrow_signed_in".as_ptr(), ptr::null_mut());
        let narrow_signed_out =
            vpi_handle_by_name(c"Top.narrow_signed_out".as_ptr(), ptr::null_mut());
        let four_in = vpi_handle_by_name(c"Top.four_in".as_ptr(), ptr::null_mut());
        let four_out = vpi_handle_by_name(c"Top.four_out".as_ptr(), ptr::null_mut());
        assert_eq!(vpi_get(VPI_SIGNED, signed_in), 1);
        assert_eq!(vpi_get(VPI_SIGNED, signed_out), 1);
        assert_eq!(vpi_get(VPI_SIGNED, unsigned_in), 0);
        assert_eq!(vpi_get(VPI_SIGNED, unsigned_out), 0);
        put_int(signed_in, -1);
        put_int(unsigned_in, -1);
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

        vpi_get_value(unsigned_out, &mut vector);
        let words = std::slice::from_raw_parts(vector.value.vector, 2);
        assert_eq!(words[0].aval as u32, u32::MAX);
        assert_eq!(words[1].aval as u32, u32::MAX);

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

        let mut forced_word = VpiVecVal {
            aval: 0x1f,
            bval: 0x10,
        };
        let forced = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData {
                vector: &mut forced_word,
            },
        };
        assert_eq!(
            vpi_put_value(four_in, &forced, ptr::null(), VPI_FORCE_FLAG),
            four_in
        );
        vpi_get_value(four_in, &mut vector);
        let forced_word = &*vector.value.vector;
        assert_eq!(forced_word.aval, 0x0f);
        assert_eq!(forced_word.bval, 0);
        assert_eq!(
            vpi_put_value(four_in, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
            four_in
        );

        for handle in [
            signed_in,
            signed_out,
            unsigned_in,
            unsigned_out,
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
static EDGE_VALUE_CHANGE_COUNT: AtomicUsize = AtomicUsize::new(0);
static EDGE_VALUE_CHANGE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
static EDGE_VALUE_CHANGE_DRIVE_DATA: AtomicBool = AtomicBool::new(false);
static EDGE_VALUE_CHANGE_OVERRIDE_CLOCK: AtomicBool = AtomicBool::new(false);
static FINISH_EDGE_CALLBACKS: AtomicUsize = AtomicUsize::new(0);
static CALLBACKS_AFTER_EDGE_FINISH: AtomicUsize = AtomicUsize::new(0);

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

unsafe extern "C" fn drive_clock_pulse(data: *mut VpiCbData) -> i32 {
    let clk = unsafe { (*data).obj };
    unsafe {
        put_int(clk, 1);
        put_int(clk, 0);
    }
    0
}

unsafe extern "C" fn drive_two_clock_posedges(data: *mut VpiCbData) -> i32 {
    let clk = unsafe { (*data).obj };
    unsafe {
        put_int(clk, 1);
        put_int(clk, 0);
        put_int(clk, 1);
    }
    0
}

unsafe extern "C" fn finish_on_clock_change(_data: *mut VpiCbData) -> i32 {
    FINISH_EDGE_CALLBACKS.fetch_add(1, Ordering::SeqCst);
    assert_eq!(vpi_control(VPI_FINISH), 1);
    0
}

unsafe extern "C" fn record_callback_after_edge_finish(_data: *mut VpiCbData) -> i32 {
    CALLBACKS_AFTER_EDGE_FINISH.fetch_add(1, Ordering::SeqCst);
    0
}

unsafe extern "C" fn record_clock_value_change(data: *mut VpiCbData) -> i32 {
    // Safety: the callback's requested integer storage is live while firing.
    let value = unsafe { (*(*data).value).value.integer } as usize;
    EDGE_VALUE_CHANGE_COUNT.fetch_add(1, Ordering::SeqCst);
    EDGE_VALUE_CHANGE_SEQUENCE
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |sequence| {
            Some(sequence * 10 + value)
        })
        .unwrap();
    0
}

unsafe extern "C" fn drive_data_after_first_clock_edge(data: *mut VpiCbData) -> i32 {
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    unsafe { vpi_get_value((*data).obj, &mut value) };
    if unsafe { value.value.integer } == 1
        && EDGE_VALUE_CHANGE_DRIVE_DATA.swap(false, Ordering::SeqCst)
    {
        let d = EDGE_DATA_HANDLE.load(Ordering::SeqCst) as VpiHandle;
        unsafe { put_int(d, 1) };
    }
    0
}

unsafe extern "C" fn override_clock_after_first_edge(data: *mut VpiCbData) -> i32 {
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    // Safety: callback data and its object handle remain live while firing.
    unsafe { vpi_get_value((*data).obj, &mut value) };
    if unsafe { value.value.integer } == 1
        && EDGE_VALUE_CHANGE_OVERRIDE_CLOCK.swap(false, Ordering::SeqCst)
    {
        unsafe { put_int((*data).obj, 0) };
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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

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
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));

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

#[test]
fn value_change_callbacks_observe_each_queued_clock_transition() {
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    EDGE_VALUE_CHANGE_COUNT.store(0, Ordering::SeqCst);
    EDGE_VALUE_CHANGE_SEQUENCE.store(0, Ordering::SeqCst);

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        let mut callback_value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        let value_change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(record_clock_value_change),
            obj: clk,
            time: ptr::null_mut(),
            value: &mut callback_value,
            index: 0,
            user_data: ptr::null_mut(),
        };
        let value_change_handle = vpi_register_cb(&value_change);
        assert!(!value_change_handle.is_null());

        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let pulse = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_clock_pulse),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&pulse).is_null());
        assert!(!run_callbacks());

        assert_eq!(EDGE_VALUE_CHANGE_COUNT.load(Ordering::SeqCst), 2);
        assert_eq!(EDGE_VALUE_CHANGE_SEQUENCE.load(Ordering::SeqCst), 10);
        assert_eq!(vpi_remove_cb(value_change_handle), 1);
        assert_eq!(vpi_free_object(clk), 1);
    }
    clear_runtime();
}

#[test]
fn inactive_clock_transition_is_replayed_between_two_active_edges() {
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    EDGE_VALUE_CHANGE_COUNT.store(0, Ordering::SeqCst);
    EDGE_VALUE_CHANGE_SEQUENCE.store(0, Ordering::SeqCst);

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        let mut callback_value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        let value_change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(record_clock_value_change),
            obj: clk,
            time: ptr::null_mut(),
            value: &mut callback_value,
            index: 0,
            user_data: ptr::null_mut(),
        };
        let value_change_handle = vpi_register_cb(&value_change);
        assert!(!value_change_handle.is_null());

        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let edges = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_two_clock_posedges),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&edges).is_null());
        assert!(!run_callbacks());

        assert_eq!(EDGE_VALUE_CHANGE_COUNT.load(Ordering::SeqCst), 3);
        assert_eq!(EDGE_VALUE_CHANGE_SEQUENCE.load(Ordering::SeqCst), 101);
        assert_eq!(vpi_remove_cb(value_change_handle), 1);
        assert_eq!(vpi_free_object(clk), 1);
    }
    clear_runtime();
}

#[test]
fn callback_writes_override_later_queued_edge_snapshots() {
    let simulator = Simulator::builder(EDGE_SAMPLER, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    EDGE_VALUE_CHANGE_DRIVE_DATA.store(true, Ordering::SeqCst);

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

        let value_change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(drive_data_after_first_clock_edge),
            obj: clk,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let value_change_handle = vpi_register_cb(&value_change);
        assert!(!value_change_handle.is_null());
        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let edges = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_two_clock_posedges),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&edges).is_null());
        assert!(!run_callbacks());

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        vpi_get_value(newest, &mut value);
        assert_eq!(value.value.integer, 1);
        vpi_get_value(previous, &mut value);
        assert_eq!(value.value.integer, 0);

        assert_eq!(vpi_remove_cb(value_change_handle), 1);
        for handle in [clk, d, newest, previous] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    EDGE_DATA_HANDLE.store(0, Ordering::SeqCst);
    clear_runtime();
}

#[test]
fn callback_clock_override_removes_a_later_queued_edge() {
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    EDGE_VALUE_CHANGE_OVERRIDE_CLOCK.store(true, Ordering::SeqCst);

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let q = vpi_handle_by_name(c"Top.q".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(q, 0);

        let value_change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(override_clock_after_first_edge),
            obj: clk,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let value_change_handle = vpi_register_cb(&value_change);
        assert!(!value_change_handle.is_null());

        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let edges = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_two_clock_posedges),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&edges).is_null());
        assert!(!run_callbacks());

        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        vpi_get_value(q, &mut value);
        assert_eq!(value.value.integer, 1);

        assert_eq!(vpi_remove_cb(value_change_handle), 1);
        for handle in [clk, q] {
            assert_eq!(vpi_free_object(handle), 1);
        }
    }
    clear_runtime();
}

#[test]
fn finish_during_queued_edge_replay_stops_remaining_callbacks_and_edges() {
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ));
    FINISH_EDGE_CALLBACKS.store(0, Ordering::SeqCst);
    CALLBACKS_AFTER_EDGE_FINISH.store(0, Ordering::SeqCst);

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        let finish = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(finish_on_clock_change),
            obj: clk,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        let after_finish = VpiCbData {
            cb_rtn: Some(record_callback_after_edge_finish),
            ..finish
        };
        assert!(!vpi_register_cb(&finish).is_null());
        assert!(!vpi_register_cb(&after_finish).is_null());

        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let edges = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_two_clock_posedges),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&edges).is_null());
        assert!(run_callbacks());

        assert_eq!(FINISH_EDGE_CALLBACKS.load(Ordering::SeqCst), 1);
        assert_eq!(CALLBACKS_AFTER_EDGE_FINISH.load(Ordering::SeqCst), 0);
        assert_eq!(vpi_free_object(clk), 1);
    }
    clear_runtime();
}
