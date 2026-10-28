use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
};

use celox::{NativeProgramInstance, SimBackend, Simulator};
use celox_vpi::*;

static VPI_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static LONG_CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);
static LONG_CALLBACK_DELAY: AtomicUsize = AtomicUsize::new(1);

#[cfg(feature = "systemverilog")]
#[test]
fn systemverilog_parameters_are_typed_read_only_objects_at_o2() {
    let _serial = lock_vpi();
    let source = "module Top #(parameter WIDTH=8, parameter signed NEG=-3,
        parameter logic [64:0] WIDE=65'h10000000000000001,
        parameter logic [3:0] FOUR=4'b10xz)(output logic [7:0] y);
        assign y=WIDTH; endmodule";
    let simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("params.sv"))], "Top")
            .param("WIDTH", 9)
            .four_state(true)
            .opt_level(celox::OptLevel::O2)
            .build()
            .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
    unsafe {
        for (name, expected) in [
            (c"Top.WIDTH", "00000000000000000000000000001001"),
            (c"Top.NEG", "11111111111111111111111111111101"),
            (
                c"Top.WIDE",
                "10000000000000000000000000000000000000000000000000000000000000001",
            ),
            (c"Top.FOUR", "10xz"),
        ] {
            let handle = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
            assert!(!handle.is_null(), "{name:?}");
            assert_eq!(vpi_get(VPI_TYPE, handle), VPI_PARAMETER);
            assert_eq!(vpi_get(VPI_CONST_TYPE, handle), VPI_BINARY_CONST);
            let mut value = VpiValue {
                format: VPI_BIN_STR_VAL,
                value: VpiValueData { integer: 0 },
            };
            vpi_get_value(handle, &mut value);
            assert_eq!(CStr::from_ptr(value.value.str_).to_str().unwrap(), expected);
            let replacement = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 0 },
            };
            vpi_put_value(handle, &replacement, ptr::null(), VPI_NO_DELAY);
            vpi_get_value(handle, &mut value);
            assert_eq!(CStr::from_ptr(value.value.str_).to_str().unwrap(), expected);
            vpi_free_object(handle);
        }
        let top = vpi_handle_by_name(c"Top".as_ptr(), ptr::null_mut());
        let iterator = vpi_iterate(VPI_PARAMETER, top);
        let mut count = 0;
        loop {
            let parameter = vpi_scan(iterator);
            if parameter.is_null() {
                break;
            }
            count += 1;
            vpi_free_object(parameter);
        }
        assert_eq!(count, 4);
        vpi_free_object(top);
    }
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
#[test]
fn interface_parameters_keep_instance_paths_and_specialized_values() {
    let _serial = lock_vpi();
    let source = r#"
        package P; parameter PKG = 123; parameter axil_ctrl_bar$not_member = 99; endpackage
        interface Bus #(parameter ADDR_W = 8, parameter extra$width = 3);
            localparam HALF = ADDR_W / 2;
            logic [ADDR_W-1:0] data;
        endinterface
        module Top #(parameter authored$width = 7)(output logic q);
            import P::*;
            Bus #(.ADDR_W(24)) axil_ctrl_bar();
            Bus #(.ADDR_W(12)) lanes[1:0]();
            assign q = 1'b0;
        endmodule
    "#;
    let simulator = Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("interface_params.sv"))],
        "Top",
    )
    .opt_level(celox::OptLevel::O1)
    .build()
    .unwrap();
    let image = simulator.shared_code().program_image().clone();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let _session = install_runtime(trusted_instance(image)).unwrap();
    unsafe {
        for (name, expected) in [
            (c"Top.axil_ctrl_bar.ADDR_W", 24),
            (c"Top.axil_ctrl_bar.HALF", 12),
            (c"Top.lanes[1].ADDR_W", 12),
            (c"Top.lanes[0].HALF", 6),
            (c"Top.lanes[0].extra$width", 3),
            (c"Top.authored$width", 7),
        ] {
            let handle = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
            assert!(!handle.is_null(), "{name:?}");
            assert_eq!(vpi_get(VPI_TYPE, handle), VPI_PARAMETER);
            let mut value = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 0 },
            };
            vpi_get_value(handle, &mut value);
            assert_eq!(value.value.integer, expected, "{name:?}");
            value.value.integer = 0;
            vpi_put_value(handle, &value, ptr::null(), VPI_NO_DELAY);
            vpi_get_value(handle, &mut value);
            assert_eq!(value.value.integer, expected, "{name:?} is read-only");
            vpi_free_object(handle);
        }
        // Imported package constants do not acquire module-instance names.
        assert!(vpi_handle_by_name(c"Top.PKG".as_ptr(), ptr::null_mut()).is_null());
        assert!(
            vpi_handle_by_name(c"Top.axil_ctrl_bar$not_member".as_ptr(), ptr::null_mut()).is_null()
        );
    }
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
#[test]
fn generate_scopes_with_only_child_instances_are_iterable() {
    let _serial = lock_vpi();
    let source = r#"
        module Leaf #(parameter P = 0)(output logic q);
            timeunit 1us; timeprecision 1ns;
            assign q = P;
        endmodule
        module Top(output logic [1:0] q);
            timeunit 1ns; timeprecision 1ps;
            for (genvar n = 0; n < 2; n++) begin : bank
                if (1) begin : shell
                    Leaf #(.P(n)) u(q[n]);
                end
            end
        endmodule
    "#;
    let simulator = Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("generate_scopes.sv"))],
        "Top",
    )
    .opt_level(celox::OptLevel::O1)
    .build()
    .unwrap();
    let image = simulator.shared_code().program_image().clone();
    let image =
        celox::NativeProgramImage::from_container_bytes(&image.to_container_bytes().unwrap())
            .unwrap();
    let _session = install_runtime(trusted_instance(image)).unwrap();
    unsafe {
        let top = vpi_handle_by_name(c"Top".as_ptr(), ptr::null_mut());
        let iterator = vpi_iterate(VPI_INTERNAL_SCOPE, top);
        assert!(!iterator.is_null());
        let mut names = Vec::new();
        loop {
            let bank = vpi_scan(iterator);
            if bank.is_null() {
                break;
            }
            names.push(
                CStr::from_ptr(vpi_get_str(VPI_NAME, bank))
                    .to_str()
                    .unwrap()
                    .to_string(),
            );
            assert_eq!(vpi_get(VPI_TIME_UNIT, bank), -9);
            let nested = vpi_iterate(VPI_INTERNAL_SCOPE, bank);
            assert!(!nested.is_null());
            let shell = vpi_scan(nested);
            assert!(!shell.is_null());
            assert_eq!(CStr::from_ptr(vpi_get_str(VPI_NAME, shell)), c"shell");
            assert_eq!(vpi_get(VPI_TIME_UNIT, shell), -9);
            assert!(vpi_scan(nested).is_null());
            let children = vpi_iterate(VPI_MODULE, shell);
            let leaf = vpi_scan(children);
            assert!(!leaf.is_null());
            assert_eq!(CStr::from_ptr(vpi_get_str(VPI_NAME, leaf)), c"u");
            assert_eq!(vpi_get(VPI_TIME_UNIT, leaf), -6);
            assert!(vpi_scan(children).is_null());
            vpi_free_object(leaf);
            vpi_free_object(shell);
            vpi_free_object(bank);
        }
        assert_eq!(names, ["bank[0]", "bank[1]"]);
        vpi_free_object(top);
    }
    clear_runtime();
}

unsafe extern "C" fn reschedule_long_callback(data: *mut VpiCbData) -> i32 {
    let count = LONG_CALLBACK_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
    let delay = LONG_CALLBACK_DELAY.load(Ordering::SeqCst);
    if delay != 0 && count == 500_010 {
        vpi_control(VPI_FINISH);
        return 0;
    }
    let mut time = VpiTime {
        type_: VPI_SIM_TIME,
        low: delay as u32,
        ..VpiTime::default()
    };
    // Safety: dispatch owns readable callback data, and registration copies time.
    let next = unsafe {
        VpiCbData {
            time: &mut time,
            ..*data
        }
    };
    unsafe {
        vpi_register_cb(&next);
    }
    0
}

#[test]
fn callback_iteration_limit_only_bounds_nonadvancing_time() {
    let _serial = lock_vpi();
    for delay in [1, 0] {
        let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
        let _session = install_runtime(trusted_instance(
            simulator.shared_code().program_image().clone(),
        ))
        .unwrap();
        LONG_CALLBACK_COUNT.store(0, Ordering::SeqCst);
        LONG_CALLBACK_DELAY.store(delay, Ordering::SeqCst);
        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            low: delay as u32,
            ..VpiTime::default()
        };
        let callback = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(reschedule_long_callback),
            obj: ptr::null_mut(),
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        unsafe {
            assert!(!vpi_register_cb(&callback).is_null());
        }
        let result = run_callbacks_result();
        if delay != 0 {
            assert!(result.unwrap());
            assert_eq!(LONG_CALLBACK_COUNT.load(Ordering::SeqCst), 500_010);
            assert_eq!(celox_vpi_current_time(), 500_010);
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("without advancing time")
            );
        }
        clear_runtime();
    }
}

fn lock_vpi() -> std::sync::MutexGuard<'static, ()> {
    VPI_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn trusted_instance(image: celox::NativeProgramImage) -> NativeProgramInstance {
    // Safety: tests execute only images produced in-process by the Celox compiler.
    unsafe { NativeProgramInstance::from_image(image) }.unwrap()
}

#[test]
fn vpi_state_capture_matches_simulator_and_preserves_visible_values() {
    let _serial = lock_vpi();
    let mut simulator = Simulator::builder(DESIGN, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let expected = simulator.save_state().unwrap();
    let instance = trusted_instance(simulator.shared_code().program_image().clone());
    assert_eq!(instance.save_state().unwrap(), expected);
    let session = install_runtime(instance).unwrap();
    let before = session.save_state().unwrap();
    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        assert!(!a.is_null());
        let value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 42 },
        };
        vpi_put_value(a, &value, ptr::null(), VPI_NO_DELAY);
        let saved = session.save_state().unwrap();
        assert_ne!(saved, before);
        assert!(saved.schedule.is_none());
        let a = saved
            .objects
            .iter()
            .find(|object| object.path == "a")
            .unwrap();
        assert_eq!(a.value, [42]);
        let mut bytes = Vec::new();
        saved.write_to(&mut bytes).unwrap();
        assert_eq!(
            celox::StateFile::read_from(bytes.as_slice()).unwrap(),
            saved
        );
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("vpi.state");
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        assert_eq!(celox_vpi_save_state(c_path.as_ptr()), 1);
        assert_eq!(
            celox::StateFile::read_from(std::fs::File::open(path).unwrap()).unwrap(),
            saved
        );
        assert_eq!(celox_vpi_save_state(ptr::null()), 0);
        // Capturing neither consumes events nor changes HDL values.
        assert_eq!(session.save_state().unwrap(), saved);
    }
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
    let _serial = lock_vpi();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COMPILE_TIME_OBJECTS, "Top")
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

    unsafe {
        let width = vpi_handle_by_name(c"Top.WIDTH".as_ptr(), ptr::null_mut());
        assert!(!width.is_null());
        assert_eq!(vpi_get(VPI_TYPE, width), VPI_PARAMETER);
        let replacement = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        assert!(vpi_put_value(width, &replacement, ptr::null(), VPI_NO_DELAY).is_null());
        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(width, &mut value);
        assert_eq!(value.value.integer, 8);
        vpi_free_object(width);
        assert!(vpi_handle_by_name(c"Top.BIAS".as_ptr(), ptr::null_mut()).is_null());
        let output = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        assert!(!output.is_null());
        assert_eq!(vpi_free_object(output), 1);
    }
    clear_runtime();
}

#[test]
fn callback_runtime_advances_time_and_finishes_regions() {
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let runtime = trusted_instance(simulator.shared_code().program_image().clone());
    let _session = install_runtime(runtime).unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
fn scaled_real_zero_delay_inertial_write_waits_for_the_update_round() {
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let initial = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 1 },
        };
        assert_eq!(vpi_put_value(a, &initial, ptr::null(), VPI_NO_DELAY), a);
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
        assert_eq!(output.value.integer, 2);
        assert!(!run_callbacks());
        vpi_get_value(y, &mut output);
        assert_eq!(output.value.integer, 42);
        assert_eq!(vpi_free_object(a), 1);
        assert_eq!(vpi_free_object(y), 1);
    }
    clear_runtime();
}

#[test]
fn inertial_deposits_coalesce_shared_storage_and_own_the_value() {
    let _serial = lock_vpi();
    // Multiple handles always share storage. The two-state lowerer also
    // collapses a connected port; four-state port conversions can retain
    // separate storage and are not the same deposited object.
    for (four_state, second_name) in [(false, c"Top.a"), (true, c"Top.a"), (false, c"Top.child.i")]
    {
        let simulator = Simulator::builder(DESIGN, "Top")
            .four_state(four_state)
            .build()
            .unwrap();
        let _session = install_runtime(trusted_instance(
            simulator.shared_code().program_image().clone(),
        ))
        .unwrap();
        // Safety: handles and value storage remain live for each VPI call.
        unsafe {
            let parent = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
            let child = vpi_handle_by_name(second_name.as_ptr(), ptr::null_mut());
            let output = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
            assert!(!parent.is_null() && !child.is_null() && !output.is_null());
            let mut value = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 0 },
            };
            assert_eq!(
                vpi_put_value(parent, &value, ptr::null(), VPI_NO_DELAY),
                parent
            );
            value.value.integer = 41;
            assert_eq!(
                vpi_put_value(parent, &value, ptr::null(), VPI_INERTIAL_DELAY),
                parent
            );
            value.value.integer = 73;
            assert_eq!(
                vpi_put_value(child, &value, ptr::null(), VPI_INERTIAL_DELAY),
                child
            );
            // The queued update owns its data, not the caller's union or handle.
            value.value.integer = 99;
            assert_eq!(vpi_free_object(parent), 1);
            vpi_get_value(child, &mut value);
            assert_eq!(value.value.integer, 0);
            assert!(!run_callbacks());
            vpi_get_value(child, &mut value);
            assert_eq!(value.value.integer, 73, "four_state={four_state}");
            vpi_get_value(output, &mut value);
            assert_eq!(value.value.integer, 74);
            assert_eq!(vpi_free_object(child), 1);
            assert_eq!(vpi_free_object(output), 1);
        }
    }
}

#[test]
fn finish_stops_remaining_callback_regions_but_still_runs_end_callbacks() {
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let container = simulator
        .shared_code()
        .program_image()
        .to_container_bytes()
        .unwrap();
    // Safety: the container was created above from an in-process compiled image.
    let runtime = unsafe { NativeProgramInstance::from_attached_bytes(&container) }.unwrap();
    drop(simulator);
    let _session = install_runtime(runtime).unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
        assert_eq!(read.value.integer, 42);
        assert!(!run_callbacks());
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(TWO_STATE_VALUES, "Top")
        .four_state(true)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top")
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(BRANCHED_COMB, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(ORDERED_BRANCHED_COMB, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(LOOPED_COMB, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(ALIASED_CLOCK, "Top")
        .opt_level(celox::OptLevel::O0)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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

// These regressions exercise native reflection and VPI writes, rather than
// portable HDL behavior, so they belong with the VPI API tests.
#[test]
fn sv_mixed_clock_reset_aliases_dispatch_both_consumers() {
    let _serial = lock_vpi();
    for falling in [false, true] {
        let source = r#"
            module ClockChild(input logic clk, input logic [7:0] d, output logic [7:0] q);
                always_ff @(CLOCK_EDGE clk) q <= d;
            endmodule
            module ResetChild(input logic clk, input logic rst, input logic [7:0] d,
                              output logic [7:0] q);
                always_ff @(posedge clk or RESET_EDGE rst)
                    if (RESET_ACTIVE) q <= 0;
                    else q <= d;
            endmodule
            module Top(input logic event_in, input logic other_clk, input logic [7:0] d,
                       output logic [7:0] clock_q, output logic [7:0] reset_q);
                ClockChild clock_child(.clk(event_in), .d(d), .q(clock_q));
                ResetChild reset_child(.clk(other_clk), .rst(event_in), .d(d), .q(reset_q));
            endmodule
        "#
        .replace("CLOCK_EDGE", if falling { "negedge" } else { "posedge" })
        .replace("RESET_EDGE", if falling { "negedge" } else { "posedge" })
        .replace("RESET_ACTIVE", if falling { "!rst" } else { "rst" });
        let simulator = Simulator::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("mixed_roles.sv"))],
            "Top",
        )
        .opt_level(celox::OptLevel::O0)
        .four_state(true)
        .build()
        .unwrap();
        let runtime = trusted_instance(simulator.shared_code().program_image().clone());
        assert_eq!(
            runtime.signal("Top.event_in").unwrap().domain_kind,
            if falling {
                celox::DomainKind::ClockNegedge
            } else {
                celox::DomainKind::ClockPosedge
            }
        );
        let _session = install_runtime(runtime).unwrap();
        unsafe {
            let event = vpi_handle_by_name(c"Top.event_in".as_ptr(), ptr::null_mut());
            let other = vpi_handle_by_name(c"Top.other_clk".as_ptr(), ptr::null_mut());
            let d = vpi_handle_by_name(c"Top.d".as_ptr(), ptr::null_mut());
            let clock_q = vpi_handle_by_name(c"Top.clock_q".as_ptr(), ptr::null_mut());
            let reset_q = vpi_handle_by_name(c"Top.reset_q".as_ptr(), ptr::null_mut());
            put_int(event, i32::from(falling));
            put_int(other, 0);
            put_int(d, 9);
            put_int(other, 1);
            let mut value = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: -1 },
            };
            vpi_get_value(reset_q, &mut value);
            assert_eq!(value.value.integer, 9);
            put_int(d, 7);
            put_int(event, i32::from(!falling));
            vpi_get_value(clock_q, &mut value);
            assert_eq!(value.value.integer, 7, "clock consumer, falling={falling}");
            vpi_get_value(reset_q, &mut value);
            assert_eq!(value.value.integer, 0, "reset consumer, falling={falling}");
            for handle in [event, other, d, clock_q, reset_q] {
                assert_eq!(vpi_free_object(handle), 1);
            }
        }
        clear_runtime();
    }
}

#[test]
fn sv_downstream_clock_copies_do_not_drive_upstream_events() {
    let _serial = lock_vpi();
    let source = r#"
        module Child(input logic clk, input logic [7:0] d, output logic [7:0] q);
            always_ff @(posedge clk) q <= d;
        endmodule
        module Top(input logic clk, input logic [7:0] d, output logic [7:0] q,
                   output logic monitor);
            logic internal_monitor;
            assign monitor = clk;
            assign internal_monitor = clk;
            Child child(.clk(clk), .d(d), .q(q));
        endmodule
    "#;
    let simulator = Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("clock_copies.sv"))],
        "Top",
    )
    .opt_level(celox::OptLevel::O0)
    .four_state(true)
    .native_force_support(true)
    .build()
    .unwrap();
    let runtime = trusted_instance(simulator.shared_code().program_image().clone());
    for name in ["Top.monitor", "Top.internal_monitor"] {
        assert_eq!(
            runtime.signal(name).unwrap().domain_kind,
            celox::DomainKind::Other
        );
    }
    let _session = install_runtime(runtime).unwrap();
    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let d = vpi_handle_by_name(c"Top.d".as_ptr(), ptr::null_mut());
        let q = vpi_handle_by_name(c"Top.q".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(d, 3);
        put_int(clk, 1);
        put_int(clk, 0);
        put_int(d, 7);
        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: -1 },
        };
        for name in [c"Top.monitor", c"Top.internal_monitor"] {
            let monitor = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
            assert!(!monitor.is_null());
            put_int(monitor, 1);
            vpi_get_value(q, &mut value);
            assert_eq!(value.value.integer, 3, "deposit must not clock the child");
            let high = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 1 },
            };
            assert_eq!(
                vpi_put_value(monitor, &high, ptr::null(), VPI_FORCE_FLAG),
                monitor
            );
            vpi_get_value(clk, &mut value);
            assert_eq!(
                value.value.integer, 0,
                "force must not reach upstream clock"
            );
            vpi_get_value(q, &mut value);
            assert_eq!(value.value.integer, 3, "force must not clock the child");
            assert_eq!(
                vpi_put_value(monitor, ptr::null(), ptr::null(), VPI_RELEASE_FLAG),
                monitor
            );
            assert_eq!(vpi_free_object(monitor), 1);
        }
        put_int(clk, 1);
        vpi_get_value(q, &mut value);
        assert_eq!(value.value.integer, 7, "the source still drives the event");
        for handle in [clk, d, q] {
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
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |sequence| {
            Some(sequence * 10 + value)
        })
        .unwrap();
    0
}

fn record_region(digit: usize) {
    REGION_ORDER
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |order| {
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(FATAL_ASSERT, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(FF_FATAL_ASSERT, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(WIDE_FOUR_STATE, "Top")
        .four_state(true)
        .native_force_support(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
static PRE_NBA_VALUE: AtomicI32 = AtomicI32::new(-1);
static POST_NBA_VALUE: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" fn record_after_nba(data: *mut VpiCbData) -> i32 {
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    unsafe { vpi_get_value((*data).obj, &mut value) };
    POST_NBA_VALUE.store(unsafe { value.value.integer }, Ordering::SeqCst);
    0
}

unsafe extern "C" fn record_before_nba(data: *mut VpiCbData) -> i32 {
    let q = unsafe { (*data).user_data } as VpiHandle;
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    unsafe { vpi_get_value(q, &mut value) };
    PRE_NBA_VALUE.store(unsafe { value.value.integer }, Ordering::SeqCst);
    let read_only = VpiCbData {
        reason: CB_READ_ONLY_SYNCH,
        cb_rtn: Some(record_after_nba),
        obj: q,
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    assert!(!unsafe { vpi_register_cb(&read_only) }.is_null());
    0
}

#[test]
fn clock_callbacks_observe_pre_nba_state_with_a_fused_optimized_kernel() {
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top")
        .opt_level(celox::OptLevel::O2)
        .build()
        .unwrap();
    let runtime = trusted_instance(simulator.shared_code().program_image().clone());
    let clock_address = runtime.signal("Top.clk").unwrap().state_address;
    assert!(
        runtime
            .backend()
            .resolve_event_opt(&clock_address)
            .is_some()
    );
    // The fix must work without retaining split FF kernels or disabling the
    // single-clock optimization just to insert an observation point.
    assert!(
        runtime
            .backend()
            .resolve_eval_only_event(&clock_address)
            .is_none()
    );
    assert!(
        runtime
            .backend()
            .resolve_apply_event(&clock_address)
            .is_none()
    );
    let _session = install_runtime(runtime).unwrap();
    PRE_NBA_VALUE.store(-1, Ordering::SeqCst);
    POST_NBA_VALUE.store(-1, Ordering::SeqCst);
    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let q = vpi_handle_by_name(c"Top.q".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(q, 0);
        let edge = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(record_before_nba),
            obj: clk,
            time: ptr::null_mut(),
            value: ptr::null_mut(),
            index: 0,
            user_data: q as *mut _,
        };
        let edge_handle = vpi_register_cb(&edge);
        assert!(!edge_handle.is_null());
        let mut time = VpiTime {
            type_: VPI_SIM_TIME,
            high: 0,
            low: 1,
            real: 0.0,
        };
        let drive = VpiCbData {
            reason: CB_AFTER_DELAY,
            cb_rtn: Some(drive_callback_object_high),
            obj: clk,
            time: &mut time,
            value: ptr::null_mut(),
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&drive).is_null());
        assert!(!run_callbacks_result().unwrap());
        assert_eq!(PRE_NBA_VALUE.load(Ordering::SeqCst), 0);
        assert_eq!(POST_NBA_VALUE.load(Ordering::SeqCst), 1);
        assert_eq!(vpi_remove_cb(edge_handle), 1);
        assert_eq!(vpi_free_object(clk), 1);
        assert_eq!(vpi_free_object(q), 1);
    }
    clear_runtime();
}

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
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |sequence| {
            Some(sequence * 10 + value)
        })
        .unwrap();
    0
}

unsafe extern "C" fn drive_data_after_first_registered_update(data: *mut VpiCbData) -> i32 {
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: -1 },
    };
    unsafe { vpi_get_value((*data).obj, &mut value) };
    if unsafe { value.value.integer } == 1
        && EDGE_VALUE_CHANGE_DRIVE_DATA.swap(false, Ordering::SeqCst)
    {
        let d = EDGE_DATA_HANDLE.load(Ordering::SeqCst) as VpiHandle;
        unsafe { put_int(d, 2) };
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(TWO_CLOCKS, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top")
        .four_state(true)
        .build()
        .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(EDGE_SAMPLER, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(EDGE_SAMPLER, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
    EDGE_VALUE_CHANGE_DRIVE_DATA.store(true, Ordering::SeqCst);

    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let d = vpi_handle_by_name(c"Top.d".as_ptr(), ptr::null_mut());
        let newest = vpi_handle_by_name(c"Top.newest".as_ptr(), ptr::null_mut());
        let previous = vpi_handle_by_name(c"Top.previous".as_ptr(), ptr::null_mut());
        put_int(clk, 0);
        put_int(d, 1);
        put_int(newest, 0);
        put_int(previous, 0);
        EDGE_DATA_HANDLE.store(d as usize, Ordering::SeqCst);

        let value_change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(drive_data_after_first_registered_update),
            // Change data after the first registered update, so the first
            // sample is unambiguously complete. A clock value-change callback
            // runs before FF evaluation and cannot establish that ordering.
            obj: newest,
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
        assert_eq!(value.value.integer, 2);
        vpi_get_value(previous, &mut value);
        assert_eq!(value.value.integer, 1);

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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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
    let _serial = lock_vpi();
    let simulator = Simulator::builder(COUNTER, "Top").build().unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
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

#[cfg(feature = "systemverilog")]
#[test]
fn vpi_time_properties_follow_the_attached_design_precision() {
    let _serial = lock_vpi();
    for (declarations, unit, expected) in [
        ("", -9, -12),
        ("timeunit 1ns; timeprecision 1fs;", -9, -15),
        ("timeunit 1us; timeprecision 100ns;", -6, -7),
    ] {
        let source = format!("module Top(output logic y); {declarations} endmodule");
        let sim =
            Simulator::from_sv_sources(vec![(&source, std::path::Path::new("time.sv"))], "Top")
                .build_native()
                .unwrap();
        let bytes = sim
            .shared_code()
            .program_image()
            .to_container_bytes()
            .unwrap();
        let image = celox::NativeProgramImage::from_container_bytes(&bytes).unwrap();
        let _session = install_runtime(trusted_instance(image)).unwrap();
        unsafe {
            let top = vpi_handle_by_name(c"Top".as_ptr(), ptr::null_mut());
            assert_eq!(vpi_get(VPI_TIME_UNIT, ptr::null_mut()), expected);
            assert_eq!(vpi_get(VPI_TIME_PRECISION, ptr::null_mut()), expected);
            assert_eq!(vpi_get(VPI_TIME_UNIT, top), unit);
            assert_eq!(vpi_get(VPI_TIME_PRECISION, top), expected);
            vpi_free_object(top);
        }
        clear_runtime();
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn vpi_time_properties_preserve_module_scales_in_native_images() {
    let _serial = lock_vpi();
    let source = r#"
        `timescale 10ns / 1ps
        module Top(output logic y);
            Coarse coarse();
            Fine fine();
        endmodule
        module Coarse(output logic value);
            timeunit 1us; timeprecision 100ns;
        endmodule
        module Fine(output logic value);
            timeunit 1ns; timeprecision 1fs;
        endmodule
    "#;
    let sim = Simulator::from_sv_sources(vec![(source, std::path::Path::new("scales.sv"))], "Top")
        .build_native()
        .unwrap();
    let bytes = sim
        .shared_code()
        .program_image()
        .to_container_bytes()
        .unwrap();
    let image = celox::NativeProgramImage::from_container_bytes(&bytes).unwrap();
    let _session = install_runtime(trusted_instance(image)).unwrap();
    unsafe {
        assert_eq!(vpi_get(VPI_TIME_UNIT, ptr::null_mut()), -15);
        assert_eq!(vpi_get(VPI_TIME_PRECISION, ptr::null_mut()), -15);
        for (name, unit, precision) in [
            (c"Top", -8, -12),
            (c"Top.coarse", -6, -7),
            (c"Top.coarse.value", -6, -7),
            (c"Top.fine", -9, -15),
            (c"Top.fine.value", -9, -15),
        ] {
            let handle = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
            assert!(!handle.is_null(), "{name:?}");
            assert_eq!(vpi_get(VPI_TIME_UNIT, handle), unit, "{name:?}");
            assert_eq!(vpi_get(VPI_TIME_PRECISION, handle), precision, "{name:?}");
            vpi_free_object(handle);
        }
    }
    clear_runtime();
}

#[test]
fn sessions_release_on_unwind_and_reject_nested_or_foreign_hosts() {
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let image = simulator.shared_code().program_image().clone();
    let session = install_runtime(trusted_instance(image.clone())).unwrap();
    assert!(install_runtime(trusted_instance(image.clone())).is_err());
    // Keep the owner's handle live while a foreign C caller attempts a read.
    let address = unsafe { vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut()) } as usize;
    assert_ne!(address, 0);
    let foreign_image = image.clone();
    std::thread::spawn(move || {
        assert!(install_runtime(trusted_instance(foreign_image)).is_err());
        // A foreign thread must not see or clear the owner's runtime.
        clear_runtime();
        unsafe {
            let mut value = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: -1 },
            };
            vpi_get_value(address as VpiHandle, &mut value);
            assert_eq!(value.value.integer, -1);
            assert!(vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut()).is_null());
        }
    })
    .join()
    .unwrap();
    unsafe {
        assert_eq!(vpi_free_object(address as VpiHandle), 1);
        let signal = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        assert!(!signal.is_null());
        vpi_free_object(signal);
    }
    drop(session);
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _session = install_runtime(trusted_instance(image.clone())).unwrap();
        panic!("exercise session cleanup");
    }));
    assert!(unwind.is_err());
    let _session = install_runtime(trusted_instance(image)).unwrap();
    assert_eq!(celox_vpi_current_time(), 0);
}

struct ReentrantClear {
    image: celox::NativeProgramImage,
    replacement_rejected: bool,
    nested_dispatch_rejected: bool,
}

unsafe extern "C" fn clear_during_callback(data: *mut VpiCbData) -> i32 {
    // Safety: the test keeps its context alive throughout callback dispatch.
    let context = unsafe { &mut *((*data).user_data.cast::<ReentrantClear>()) };
    context.nested_dispatch_rejected = run_callbacks_result().is_err();
    clear_runtime();
    context.replacement_rejected =
        install_runtime(trusted_instance(context.image.clone())).is_err();
    0
}

#[test]
fn callback_teardown_keeps_its_host_selected_until_dispatch_returns() {
    let _serial = lock_vpi();
    let simulator = Simulator::builder(DESIGN, "Top").build().unwrap();
    let image = simulator.shared_code().program_image().clone();
    let session = install_runtime(trusted_instance(image.clone())).unwrap();
    let mut context = ReentrantClear {
        image: image.clone(),
        replacement_rejected: false,
        nested_dispatch_rejected: false,
    };
    let callback = VpiCbData {
        reason: CB_START_OF_SIMULATION,
        cb_rtn: Some(clear_during_callback),
        obj: ptr::null_mut(),
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: std::ptr::from_mut(&mut context).cast(),
    };
    unsafe {
        assert!(!vpi_register_cb(&callback).is_null());
    }
    assert!(run_callbacks_result().unwrap());
    assert!(context.replacement_rejected);
    assert!(context.nested_dispatch_rejected);
    // The old guard can outlive an explicitly cleared registration.
    let replacement = install_runtime(trusted_instance(image)).unwrap();
    drop(session);
    unsafe {
        let signal = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        assert!(!signal.is_null());
        vpi_free_object(signal);
    }
    drop(replacement);
    unsafe {
        assert!(vpi_register_cb(&callback).is_null());
        let mut info = VpiVlogInfo {
            argc: 0,
            argv: ptr::null_mut(),
            product: ptr::null_mut(),
            version: ptr::null_mut(),
        };
        assert_eq!(vpi_get_vlog_info(&mut info), 0);
        assert!(info.argv.is_null());
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn interface_members_are_navigable_after_native_image_round_trip() {
    let _serial = lock_vpi();
    let source = r#"
        interface Axis;
            logic [7:0] data, cost$usd;
            logic valid, ready;
            modport sink(input data, valid, output ready);
        endinterface
        module Child(Axis.sink s_axis, output logic [7:0] y);
            assign s_axis.ready = 1;
            assign y = s_axis.valid ? s_axis.data + 1 : 0;
        endmodule
        module Top(output logic [7:0] y);
            Axis s_axis(), s$axis(), lanes[2](), shifted[-1:1]();
            assign lanes[0].data=8'h34;
            assign lanes[1].data=8'h56;
            assign shifted[-1].data=8'h78;
            logic [7:0] cash$flow;
            assign cash$flow = 3;
            Child uut(s_axis, y);
        endmodule
    "#;
    let simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("axis.sv"))], "Top")
            .native_force_support(true)
            .four_state(true)
            .build_native()
            .unwrap();
    let container = simulator
        .shared_code()
        .program_image()
        .to_container_bytes()
        .unwrap();
    let runtime = unsafe { NativeProgramInstance::from_attached_bytes(&container) }.unwrap();
    drop(simulator);
    let _session = install_runtime(runtime).unwrap();
    unsafe {
        let top = vpi_handle_by_name(c"Top".as_ptr(), ptr::null_mut());
        for (name, expected) in [
            (c"Top.lanes[0].data", 0x34),
            (c"Top.lanes[1].data", 0x56),
            (c"Top.shifted[-1].data", 0x78),
        ] {
            let member = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
            assert!(!member.is_null(), "{name:?}");
            let mut value = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 0 },
            };
            vpi_get_value(member, &mut value);
            assert_eq!(value.value.integer, expected);
            vpi_free_object(member);
        }
        let bus = vpi_handle_by_name(c"s_axis".as_ptr(), top);
        assert!(!bus.is_null(), "the declared interface must have a scope");
        let data = vpi_handle_by_name(c"data".as_ptr(), bus);
        let valid = vpi_handle_by_name(c"valid".as_ptr(), bus);
        let ready = vpi_handle_by_name(c"ready".as_ptr(), bus);
        assert!(!data.is_null() && !valid.is_null() && !ready.is_null());
        assert_eq!(CStr::from_ptr(vpi_get_str(VPI_NAME, data)), c"data");
        assert_eq!(
            CStr::from_ptr(vpi_get_str(VPI_FULL_NAME, data)),
            c"Top.s_axis.data"
        );
        assert_eq!(vpi_get(VPI_SIZE, data), 8);
        let parent = vpi_handle(VPI_SCOPE, data);
        assert_eq!(
            CStr::from_ptr(vpi_get_str(VPI_FULL_NAME, parent)),
            c"Top.s_axis"
        );
        let child_data = vpi_handle_by_name(c"Top.uut.s_axis.data".as_ptr(), ptr::null_mut());
        assert!(
            !child_data.is_null(),
            "interface ports must retain their paths too"
        );
        put_int(data, 41);
        put_int(valid, 1);
        let y = vpi_handle_by_name(c"y".as_ptr(), top);
        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(y, &mut value);
        assert_eq!(value.value.integer, 42);
        vpi_get_value(child_data, &mut value);
        assert_eq!(
            value.value.integer, 41,
            "child member alias must see the deposited value"
        );
        let members = vpi_iterate(VPI_REG, bus);
        let mut names = Vec::new();
        loop {
            let member = vpi_scan(members);
            if member.is_null() {
                break;
            }
            names.push(
                CStr::from_ptr(vpi_get_str(VPI_NAME, member))
                    .to_str()
                    .unwrap()
                    .to_owned(),
            );
            vpi_free_object(member);
        }
        names.sort();
        assert_eq!(names, ["cost$usd", "data", "ready", "valid"]);
        let dollar_member = vpi_handle_by_name(c"cost$usd".as_ptr(), bus);
        let dollar_interface = vpi_handle_by_name(c"Top.s$axis.data".as_ptr(), ptr::null_mut());
        let dollar_signal = vpi_handle_by_name(c"Top.cash$flow".as_ptr(), ptr::null_mut());
        assert!(
            !dollar_member.is_null() && !dollar_interface.is_null() && !dollar_signal.is_null()
        );
        assert!(vpi_handle_by_name(c"Top.cash".as_ptr(), ptr::null_mut()).is_null());
        for handle in [
            top,
            bus,
            data,
            valid,
            ready,
            parent,
            child_data,
            y,
            dollar_member,
            dollar_interface,
            dollar_signal,
        ] {
            vpi_free_object(handle);
        }
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn two_state_image_converts_unknown_logic_deposits_and_forces() {
    let _serial = lock_vpi();
    let source = "module Top(input logic[3:0] a,output logic[3:0] y); assign y=a; endmodule";
    let simulator =
        Simulator::from_sv_sources(vec![(source, std::path::Path::new("two_state.sv"))], "Top")
            .four_state(false)
            .native_force_support(true)
            .build()
            .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
    unsafe {
        let a = vpi_handle_by_name(c"Top.a".as_ptr(), ptr::null_mut());
        let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
        let mut word = VpiVecVal { aval: 15, bval: 10 };
        let input = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData { vector: &mut word },
        };
        let mut output = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData { integer: 0 },
        };
        assert_eq!(vpi_put_value(a, &input, ptr::null(), VPI_NO_DELAY), a);
        vpi_get_value(y, &mut output);
        assert_eq!((*output.value.vector).aval, 5);
        assert_eq!((*output.value.vector).bval, 0);
        let mut force_word = VpiVecVal { aval: 15, bval: 5 };
        let force_value = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData {
                vector: &mut force_word,
            },
        };
        assert_eq!(
            vpi_put_value(a, &force_value, ptr::null(), VPI_FORCE_FLAG),
            a
        );
        vpi_get_value(y, &mut output);
        assert_eq!((*output.value.vector).aval, 10);
        assert_eq!((*output.value.vector).bval, 0);
        let mut deposited_word = VpiVecVal { aval: 15, bval: 12 };
        let deposited_value = VpiValue {
            format: VPI_VECTOR_VAL,
            value: VpiValueData {
                vector: &mut deposited_word,
            },
        };
        assert_eq!(
            vpi_put_value(a, &deposited_value, ptr::null(), VPI_NO_DELAY),
            a
        );
        assert_eq!(vpi_put_value(a, &input, ptr::null(), VPI_RELEASE_FLAG), a);
        vpi_get_value(y, &mut output);
        assert_eq!((*output.value.vector).aval, 3);
        assert_eq!((*output.value.vector).bval, 0);
        vpi_free_object(a);
        vpi_free_object(y);
    }
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
#[test]
fn unpacked_array_handles_preserve_declared_indices_and_neighbor_values() {
    let _serial = lock_vpi();
    let source = r#"module Top(
        input logic [7:0] words [3:2][-1:1],
        output logic [7:0] first, middle, last
    );
        assign first=words[3][-1];
        assign middle=words[2][0];
        assign last=words[2][1];
    endmodule"#;
    for (four_state, width) in [false, true]
        .into_iter()
        .flat_map(|four| [1, 8, 9, 65].map(|width| (four, width)))
    {
        let source = source.replace("[7:0]", &format!("[{}:0]", width - 1));
        let simulator = Simulator::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("arrays.sv"))],
            "Top",
        )
        .four_state(four_state)
        .opt_level(celox::OptLevel::O1)
        .build()
        .unwrap();
        let bytes = simulator
            .shared_code()
            .program_image()
            .to_container_bytes()
            .unwrap();
        let runtime = unsafe { NativeProgramInstance::from_attached_bytes(&bytes) }.unwrap();
        let metadata = runtime.signal("Top.words").unwrap();
        assert_eq!(metadata.unpacked_ranges, [(3, 2), (-1, 1)]);
        assert_eq!(runtime.reflection().signals().len(), 4);
        let _session = install_runtime(runtime).unwrap();
        unsafe {
            let array = vpi_handle_by_name(c"Top.words".as_ptr(), ptr::null_mut());
            assert_eq!(vpi_get(VPI_TYPE, array), VPI_REG_ARRAY);
            assert_eq!(vpi_get(VPI_SIZE, array), 2);
            assert!(vpi_handle_by_index(array, 1).is_null());
            for (kind, expected) in [(VPI_LEFT_RANGE, 3), (VPI_RIGHT_RANGE, 2)] {
                let bound = vpi_handle(kind, array);
                let mut value = VpiValue {
                    format: VPI_INT_VAL,
                    value: VpiValueData { integer: 0 },
                };
                vpi_get_value(bound, &mut value);
                assert_eq!(value.value.integer, expected);
                vpi_free_object(bound);
            }
            let row = vpi_handle_by_index(array, 2);
            assert_eq!(vpi_get(VPI_SIZE, row), 3);
            assert!(vpi_handle_by_index(row, -2).is_null());
            let middle = vpi_handle_by_index(row, 0);
            let last = vpi_handle_by_index(row, 1);
            let first = vpi_handle_by_name(c"Top.words[3][-1]".as_ptr(), ptr::null_mut());
            assert!(!first.is_null());
            assert_eq!(vpi_get(VPI_TYPE, middle), VPI_REG);
            assert_eq!(vpi_get(VPI_SIZE, middle), width as i32);
            assert_eq!(
                CStr::from_ptr(vpi_get_str(VPI_FULL_NAME, middle)),
                c"Top.words[2][0]"
            );
            let parent = vpi_handle(VPI_PARENT, middle);
            assert_eq!(
                CStr::from_ptr(vpi_get_str(VPI_FULL_NAME, parent)),
                c"Top.words"
            );
            assert!(vpi_handle(VPI_PARENT, array).is_null());
            let val = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 0xa5 },
            };
            assert!(!vpi_put_value(first, &val, ptr::null(), VPI_NO_DELAY).is_null());
            assert!(!vpi_put_value(last, &val, ptr::null(), VPI_NO_DELAY).is_null());
            let xz = VpiValue {
                format: VPI_BIN_STR_VAL,
                value: VpiValueData {
                    str_: if width == 1 { c"x" } else { c"10xz0101" }
                        .as_ptr()
                        .cast_mut(),
                },
            };
            assert!(!vpi_put_value(middle, &xz, ptr::null(), VPI_NO_DELAY).is_null());
            assert!(vpi_put_value(array, &val, ptr::null(), VPI_NO_DELAY).is_null());
            assert!(vpi_put_value(middle, &val, ptr::null(), VPI_FORCE_FLAG).is_null());
            for (name, expected) in [
                (c"Top.first", if width == 1 { "1" } else { "10100101" }),
                (c"Top.last", if width == 1 { "1" } else { "10100101" }),
                (
                    c"Top.middle",
                    match (four_state, width) {
                        (true, 1) => "x",
                        (false, 1) => "0",
                        (true, _) => "10xz0101",
                        (false, _) => "10000101",
                    },
                ),
            ] {
                let output = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
                let mut value = VpiValue {
                    format: VPI_BIN_STR_VAL,
                    value: VpiValueData { integer: 0 },
                };
                vpi_get_value(output, &mut value);
                assert_eq!(
                    CStr::from_ptr(value.value.str_).to_str().unwrap(),
                    format!("{expected:0>width$}"),
                    "{name:?}, width={width}"
                );
                vpi_free_object(output);
            }
            for handle in [array, row, middle, last, first, parent] {
                vpi_free_object(handle);
            }
        }
        clear_runtime();
    }
}

#[cfg(feature = "systemverilog")]
static TIMED_EDGES: std::sync::Mutex<Vec<(u64, i32)>> = std::sync::Mutex::new(Vec::new());

/// Records the count at every rise of the DUT's clock.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn record_timed_edge(data: *mut VpiCbData) -> i32 {
    // Safety: the callback runtime passes back the registration storage, and
    // its value is an integer.
    if unsafe { (*(*data).value).value.integer } == 1 {
        let mut count = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        // Safety: VPI calls from a callback on the session's thread.
        unsafe {
            let handle = vpi_handle_by_name(c"Top.count".as_ptr(), ptr::null_mut());
            vpi_get_value(handle, &mut count);
            vpi_free_object(handle);
        }
        // Safety: `count` holds the integer just read.
        let count = unsafe { count.value.integer };
        TIMED_EDGES
            .lock()
            .unwrap()
            .push((celox_vpi_current_time(), count));
    }
    0
}

/// A timed runtime runs the DUT's processes between the callbacks: its own
/// clock, whose rises a value-change callback sees before the register
/// updates, and its `$finish`, which ends the run.
#[cfg(feature = "systemverilog")]
#[test]
fn a_timed_runtime_runs_the_dut_processes_between_callbacks() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(output logic [7:0] count);
            logic clk = 0;
            always #5 clk = ~clk;
            initial count = 0;
            always_ff @(posedge clk) count <= count + 1;
            initial #23 $finish;
        endmodule";
    let sources = || vec![(source, std::path::Path::new("timed.sv"))];
    let image = celox::Simulation::from_sv_sources(sources(), "Top")
        .compile_native()
        .unwrap()
        .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let _session = install_timed_runtime(image, simulation).unwrap();
    TIMED_EDGES.lock().unwrap().clear();
    let mut time = VpiTime {
        type_: VPI_SIM_TIME,
        ..VpiTime::default()
    };
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: 0 },
    };
    // Safety: VPI calls on the session's thread with live callback storage.
    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        assert!(!clk.is_null());
        let change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(record_timed_edge),
            obj: clk,
            time: &mut time,
            value: &mut value,
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&change).is_null());
    }
    assert!(run_callbacks_result().unwrap());
    assert_eq!(celox_vpi_current_time(), 23);
    assert_eq!(*TIMED_EDGES.lock().unwrap(), vec![(5, 0), (15, 1)]);
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
static SPIN_WRITES: AtomicBool = AtomicBool::new(false);

/// Re-registers itself in the ReadWrite region, writing `Top.x` there when
/// [`SPIN_WRITES`] is set: a time step that never ends.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn spin_read_write(_data: *mut VpiCbData) -> i32 {
    // Safety: VPI calls from a callback on the session's thread.
    unsafe {
        if SPIN_WRITES.load(Ordering::SeqCst) {
            let x = vpi_handle_by_name(c"Top.x".as_ptr(), ptr::null_mut());
            let mut value = VpiValue {
                format: VPI_INT_VAL,
                value: VpiValueData { integer: 0 },
            };
            vpi_get_value(x, &mut value);
            value.value.integer ^= 1;
            vpi_put_value(x, &value, ptr::null(), VPI_NO_DELAY);
            vpi_free_object(x);
        }
        register_spin();
    }
    0
}

#[cfg(feature = "systemverilog")]
unsafe fn register_spin() {
    let data = VpiCbData {
        reason: CB_READ_WRITE_SYNCH,
        cb_rtn: Some(spin_read_write),
        obj: ptr::null_mut(),
        time: ptr::null_mut(),
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    // Safety: the registration copies `data`.
    assert!(!unsafe { vpi_register_cb(&data) }.is_null());
}

/// A time step whose ReadWrite callbacks never let it end fails the run,
/// whether they write or not.
#[cfg(feature = "systemverilog")]
#[test]
fn a_timed_time_step_that_never_ends_fails_the_run() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(input logic x, output logic y);
            assign y = x;
            initial #5 $display(\"never\");
        endmodule";
    for writes in [false, true] {
        let image = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("spin.sv"))],
            "Top",
        )
        .compile_native()
        .unwrap()
        .into_program_image();
        let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
            .build_native_from_image(image.clone())
            .unwrap();
        let _session = install_timed_runtime(image, simulation).unwrap();
        SPIN_WRITES.store(writes, Ordering::SeqCst);
        // Safety: VPI registration on the session's thread.
        unsafe { register_spin() };
        let error = run_callbacks_result().unwrap_err();
        assert!(
            error.contains("simulation does not advance at time 0"),
            "{error}"
        );
        assert_eq!(celox_vpi_current_time(), 0);
        clear_runtime();
    }
}

#[cfg(feature = "systemverilog")]
static FINISH_EVENTS: AtomicI32 = AtomicI32::new(0);

/// Finishes the run at the first rise of the DUT's clock.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn finish_at_the_first_rise(data: *mut VpiCbData) -> i32 {
    // Safety: the callback runtime passes back the registration storage, and
    // its value is an integer.
    if unsafe { (*(*data).value).value.integer } == 1 {
        FINISH_EVENTS.fetch_or(1, Ordering::SeqCst);
        assert_eq!(vpi_control(VPI_FINISH), 1);
    }
    0
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn record_end_of_simulation(_data: *mut VpiCbData) -> i32 {
    FINISH_EVENTS.fetch_or(2, Ordering::SeqCst);
    0
}

/// `vpiFinish` from a value-change callback inside a time step ends the run
/// at that time: the design runs nothing later, and the end-of-simulation
/// callbacks still run.
#[cfg(feature = "systemverilog")]
#[test]
fn a_finish_inside_a_time_step_ends_a_timed_run() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(output logic [7:0] count);
            logic clk = 0;
            always #5 clk = ~clk;
            initial count = 0;
            always_ff @(posedge clk) count <= count + 1;
        endmodule";
    let image =
        celox::Simulation::from_sv_sources(vec![(source, std::path::Path::new("stop.sv"))], "Top")
            .compile_native()
            .unwrap()
            .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let _session = install_timed_runtime(image, simulation).unwrap();
    FINISH_EVENTS.store(0, Ordering::SeqCst);
    let mut time = VpiTime {
        type_: VPI_SIM_TIME,
        ..VpiTime::default()
    };
    let mut value = VpiValue {
        format: VPI_INT_VAL,
        value: VpiValueData { integer: 0 },
    };
    // Safety: VPI calls on the session's thread with live callback storage.
    unsafe {
        let clk = vpi_handle_by_name(c"Top.clk".as_ptr(), ptr::null_mut());
        let change = VpiCbData {
            reason: CB_VALUE_CHANGE,
            cb_rtn: Some(finish_at_the_first_rise),
            obj: clk,
            time: &mut time,
            value: &mut value,
            index: 0,
            user_data: ptr::null_mut(),
        };
        assert!(!vpi_register_cb(&change).is_null());
        let end = VpiCbData {
            reason: CB_END_OF_SIMULATION,
            cb_rtn: Some(record_end_of_simulation),
            obj: ptr::null_mut(),
            value: ptr::null_mut(),
            ..change
        };
        assert!(!vpi_register_cb(&end).is_null());
    }
    assert!(run_callbacks_result().unwrap());
    assert_eq!(celox_vpi_current_time(), 5);
    assert_eq!(FINISH_EVENTS.load(Ordering::SeqCst), 3);
    // The callback ends the run before the triggered domain can commit.
    assert_eq!(unsafe { read_int(c"Top.count") }, 0);
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
static READ_ONLY_EVENTS: std::sync::Mutex<Vec<(&'static str, u64, bool)>> =
    std::sync::Mutex::new(Vec::new());

/// Tries to write `Top.x`; returns whether the write was accepted.
#[cfg(feature = "systemverilog")]
unsafe fn try_write_x() -> bool {
    // Safety: VPI calls from a callback on the session's thread.
    unsafe {
        let x = vpi_handle_by_name(c"Top.x".as_ptr(), ptr::null_mut());
        let value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 1 },
        };
        let accepted = !vpi_put_value(x, &value, ptr::null(), VPI_NO_DELAY).is_null();
        vpi_free_object(x);
        accepted
    }
}

#[cfg(feature = "systemverilog")]
unsafe fn register_read_only_probe(
    reason: i32,
    callback: unsafe extern "C" fn(*mut VpiCbData) -> i32,
) {
    let mut delay = VpiTime {
        type_: VPI_SIM_TIME,
        ..VpiTime::default()
    };
    let data = VpiCbData {
        reason,
        cb_rtn: Some(callback),
        obj: ptr::null_mut(),
        time: &mut delay,
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    // Safety: the registration copies `data` and `delay`.
    assert!(!unsafe { vpi_register_cb(&data) }.is_null());
}

/// Records a ReadOnly write attempt and schedules a zero-delay timer.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn read_only_writes(_data: *mut VpiCbData) -> i32 {
    // Safety: VPI calls from a callback on the session's thread.
    let accepted = unsafe { try_write_x() };
    READ_ONLY_EVENTS
        .lock()
        .unwrap()
        .push(("read-only", celox_vpi_current_time(), accepted));
    // Safety: as above.
    unsafe { register_read_only_probe(CB_AFTER_DELAY, zero_delay_after_read_only) };
    0
}

/// A zero-delay timer from ReadOnly: still read-only, and the ReadWrite it
/// registers belongs to a later time step.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn zero_delay_after_read_only(_data: *mut VpiCbData) -> i32 {
    // Safety: VPI calls from a callback on the session's thread.
    let accepted = unsafe { try_write_x() };
    READ_ONLY_EVENTS
        .lock()
        .unwrap()
        .push(("timer", celox_vpi_current_time(), accepted));
    // Safety: as above.
    unsafe { register_read_only_probe(CB_READ_WRITE_SYNCH, read_write_after_read_only) };
    0
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn read_write_after_read_only(_data: *mut VpiCbData) -> i32 {
    // Safety: VPI calls from a callback on the session's thread.
    let accepted = unsafe { try_write_x() };
    READ_ONLY_EVENTS
        .lock()
        .unwrap()
        .push(("read-write", celox_vpi_current_time(), accepted));
    0
}

/// A timed runtime rejects writes in ReadOnly (IEEE 1800-2023 38.36.2), and
/// a zero-delay timer registered there runs without opening the time
/// step's regions again.
#[cfg(feature = "systemverilog")]
#[test]
fn a_timed_read_only_region_rejects_writes_and_stays_closed() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(input logic x, output logic y);
            assign y = x;
            initial #10 $display(\"later\");
        endmodule";
    let image = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("read_only.sv"))],
        "Top",
    )
    .compile_native()
    .unwrap()
    .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let _session = install_timed_runtime(image, simulation).unwrap();
    READ_ONLY_EVENTS.lock().unwrap().clear();
    // Safety: VPI registration on the session's thread.
    unsafe { register_read_only_probe(CB_READ_ONLY_SYNCH, read_only_writes) };
    assert!(!run_callbacks_result().unwrap());
    assert_eq!(
        *READ_ONLY_EVENTS.lock().unwrap(),
        vec![
            ("read-only", 0, false),
            ("timer", 0, false),
            ("read-write", 10, true)
        ]
    );
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
static ZERO_DELAY_EVENTS: std::sync::Mutex<Vec<(&'static str, u64, i32)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(feature = "systemverilog")]
unsafe fn read_int(name: &CStr) -> i32 {
    // Safety: VPI calls on the session's thread.
    unsafe {
        let handle = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
        let mut value = VpiValue {
            format: VPI_INT_VAL,
            value: VpiValueData { integer: 0 },
        };
        vpi_get_value(handle, &mut value);
        vpi_free_object(handle);
        value.value.integer
    }
}

#[cfg(feature = "systemverilog")]
fn zero_delay_event(name: &'static str) {
    // Safety: VPI calls from a callback on the session's thread.
    let y = unsafe { read_int(c"Top.y") };
    ZERO_DELAY_EVENTS
        .lock()
        .unwrap()
        .push((name, celox_vpi_current_time(), y));
}

/// The timer at 5 registers a zero-delay timer.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn timer_at_five(_data: *mut VpiCbData) -> i32 {
    zero_delay_event("timer");
    // Safety: VPI registration from a callback on the session's thread.
    unsafe { register_read_only_probe(CB_AFTER_DELAY, first_zero_delay) };
    0
}

/// Writes `x` and registers a ReadWrite callback.
#[cfg(feature = "systemverilog")]
unsafe extern "C" fn first_zero_delay(_data: *mut VpiCbData) -> i32 {
    zero_delay_event("first zero delay");
    // Safety: VPI calls from a callback on the session's thread.
    unsafe {
        assert!(try_write_x());
        register_read_only_probe(CB_READ_WRITE_SYNCH, read_write_registers_a_timer);
    }
    0
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn read_write_registers_a_timer(_data: *mut VpiCbData) -> i32 {
    zero_delay_event("read-write");
    // Safety: VPI registration from a callback on the session's thread.
    unsafe { register_read_only_probe(CB_AFTER_DELAY, second_zero_delay) };
    0
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn second_zero_delay(_data: *mut VpiCbData) -> i32 {
    zero_delay_event("second zero delay");
    // Safety: VPI registration from a callback on the session's thread.
    unsafe { register_read_only_probe(CB_READ_ONLY_SYNCH, read_only_after_timers) };
    0
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn read_only_after_timers(_data: *mut VpiCbData) -> i32 {
    zero_delay_event("read-only");
    0
}

/// Zero-delay timers that timer and ReadWrite callbacks register run in the
/// same time step, before its ReadOnly region closes it.
#[cfg(feature = "systemverilog")]
#[test]
fn zero_delay_timers_run_before_a_timed_step_closes() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(input logic x, output logic y);
            assign y = x;
            initial #20 $display(\"later\");
        endmodule";
    let image = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("zero_delay.sv"))],
        "Top",
    )
    .compile_native()
    .unwrap()
    .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let _session = install_timed_runtime(image, simulation).unwrap();
    ZERO_DELAY_EVENTS.lock().unwrap().clear();
    let mut delay = VpiTime {
        type_: VPI_SIM_TIME,
        low: 5,
        ..VpiTime::default()
    };
    let timer = VpiCbData {
        reason: CB_AFTER_DELAY,
        cb_rtn: Some(timer_at_five),
        obj: ptr::null_mut(),
        time: &mut delay,
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    // Safety: VPI registration on the session's thread.
    assert!(!unsafe { vpi_register_cb(&timer) }.is_null());
    assert!(!run_callbacks_result().unwrap());
    assert_eq!(
        *ZERO_DELAY_EVENTS.lock().unwrap(),
        vec![
            ("timer", 5, 0),
            ("first zero delay", 5, 0),
            ("read-write", 5, 1),
            ("second zero delay", 5, 1),
            ("read-only", 5, 1),
        ]
    );
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
static FLAG_AT_END: AtomicI32 = AtomicI32::new(-1);

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn finish_from_a_timer(_data: *mut VpiCbData) -> i32 {
    assert_eq!(vpi_control(VPI_FINISH), 1);
    0
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn record_flag_at_end(_data: *mut VpiCbData) -> i32 {
    // Safety: VPI calls from a callback on the session's thread.
    FLAG_AT_END.store(unsafe { read_int(c"Top.flag") }, Ordering::SeqCst);
    0
}

/// `vpiFinish` from a due timer ends a timed run before the design's slot
/// at that time runs.
#[cfg(feature = "systemverilog")]
#[test]
fn a_finish_from_a_timer_stops_before_the_designs_slot() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(output logic flag);
            initial begin
                flag = 0;
                #5 flag = 1;
            end
        endmodule";
    let image = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("timer_finish.sv"))],
        "Top",
    )
    .compile_native()
    .unwrap()
    .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let _session = install_timed_runtime(image, simulation).unwrap();
    FLAG_AT_END.store(-1, Ordering::SeqCst);
    let mut delay = VpiTime {
        type_: VPI_SIM_TIME,
        low: 5,
        ..VpiTime::default()
    };
    let timer = VpiCbData {
        reason: CB_AFTER_DELAY,
        cb_rtn: Some(finish_from_a_timer),
        obj: ptr::null_mut(),
        time: &mut delay,
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    let end = VpiCbData {
        reason: CB_END_OF_SIMULATION,
        cb_rtn: Some(record_flag_at_end),
        ..timer
    };
    // Safety: VPI registration on the session's thread.
    unsafe {
        assert!(!vpi_register_cb(&timer).is_null());
        assert!(!vpi_register_cb(&end).is_null());
    }
    assert!(run_callbacks_result().unwrap());
    assert_eq!(celox_vpi_current_time(), 5);
    assert_eq!(FLAG_AT_END.load(Ordering::SeqCst), 0);
    clear_runtime();
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn write_plain_data_input(_data: *mut VpiCbData) -> i32 {
    // Safety: VPI calls on the session's thread.
    assert!(unsafe { try_write_x() });
    0
}

#[cfg(feature = "systemverilog")]
fn settle_plain_data_input(action: &str) -> Result<bool, String> {
    let source = format!(
        r#"`timescale 1ns/1ns
        module Top(input logic x, output logic y);
            assign y = x;
            always_comb if (x) begin {action} end
            initial #10 $finish;
        endmodule"#
    );
    let image = celox::Simulation::from_sv_sources(
        vec![(source.as_str(), std::path::Path::new("fallback_settle.sv"))],
        "Top",
    )
    .compile_native()
    .unwrap()
    .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let _session = install_timed_runtime(image, simulation).unwrap();
    let mut delay = VpiTime {
        type_: VPI_SIM_TIME,
        low: 5,
        ..VpiTime::default()
    };
    let timer = VpiCbData {
        reason: CB_AFTER_DELAY,
        cb_rtn: Some(write_plain_data_input),
        obj: ptr::null_mut(),
        time: &mut delay,
        value: ptr::null_mut(),
        index: 0,
        user_data: ptr::null_mut(),
    };
    // Safety: registration copies live callback data on the session's thread.
    unsafe {
        assert!(!vpi_register_cb(&timer).is_null());
    }
    let result = run_callbacks_result();
    assert_eq!(celox_vpi_current_time(), 5);
    clear_runtime();
    result
}

#[cfg(feature = "systemverilog")]
#[test]
fn a_plain_input_settle_finishes_cleanly() {
    let _serial = lock_vpi();
    assert!(settle_plain_data_input("$finish;").unwrap());
}

#[cfg(feature = "systemverilog")]
#[test]
fn a_plain_input_settle_reports_fatal_errors() {
    let _serial = lock_vpi();
    let error = settle_plain_data_input("$fatal(1, \"fallback failure\");").unwrap_err();
    assert!(error.contains("fallback failure"), "{error}");
}

#[cfg(feature = "systemverilog")]
static TIMED_CAPTURE_STATUS: AtomicI32 = AtomicI32::new(0);

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn capture_timed_read_only(data: *mut VpiCbData) -> i32 {
    // Safety: the test keeps its CString alive until callback dispatch returns.
    let result = unsafe { celox_vpi_save_state((*data).user_data) };
    TIMED_CAPTURE_STATUS.store(result, Ordering::SeqCst);
    0
}

#[cfg(feature = "systemverilog")]
#[test]
fn timed_vpi_capture_reads_the_active_slot_without_advancing_it() {
    let _serial = lock_vpi();
    let source = "module Top(output int count); initial begin count=7; #1 count=9; #1 $finish; end endmodule";
    let image = celox::Simulation::from_sv_sources(
        vec![(source, std::path::Path::new("capture.sv"))],
        "Top",
    )
    .compile_native()
    .unwrap()
    .into_program_image();
    let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
        .build_native_from_image(image.clone())
        .unwrap();
    let session = install_timed_runtime(image, simulation).unwrap();
    assert!(session.save_state().unwrap().schedule.is_none());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("timed.state");
    let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    let mut time = VpiTime {
        type_: VPI_SIM_TIME,
        ..VpiTime::default()
    };
    let callback = VpiCbData {
        reason: CB_READ_ONLY_SYNCH,
        cb_rtn: Some(capture_timed_read_only),
        obj: ptr::null_mut(),
        time: &mut time,
        value: ptr::null_mut(),
        index: 0,
        user_data: c_path.as_ptr().cast_mut(),
    };
    TIMED_CAPTURE_STATUS.store(0, Ordering::SeqCst);
    unsafe {
        assert!(!vpi_register_cb(&callback).is_null());
    }
    assert!(run_callbacks_result().unwrap());
    assert_eq!(TIMED_CAPTURE_STATUS.load(Ordering::SeqCst), 1);
    let saved = celox::StateFile::read_from(std::fs::File::open(path).unwrap()).unwrap();
    assert!(saved.schedule.is_none());
    assert_eq!(
        saved
            .objects
            .iter()
            .find(|o| o.path == "count")
            .unwrap()
            .value,
        [7, 0, 0, 0]
    );
    let final_state = session.save_state().unwrap();
    assert_eq!(
        final_state
            .objects
            .iter()
            .find(|o| o.path == "count")
            .unwrap()
            .value,
        [9, 0, 0, 0]
    );
}

/// Host deposits after quiescence reopen update regions without advancing time.
#[cfg(feature = "systemverilog")]
#[test]
fn inertial_deposits_resume_a_quiescent_timed_session() {
    let _serial = lock_vpi();
    let source = "`timescale 1ns/1ns
        module Top(input logic [7:0] x, output logic [7:0] y, output logic started);
            initial started = 1;
            assign y = x + 1;
        endmodule";
    for four_state in [false, true] {
        let image = celox::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("resume_inertial.sv"))],
            "Top",
        )
        .four_state(four_state)
        .opt_level(celox::OptLevel::O1)
        .compile_native()
        .unwrap()
        .into_program_image();
        let simulation = celox::Simulation::from_sv_sources(Vec::new(), "")
            .build_native_from_image(image.clone())
            .unwrap();
        let _session = install_timed_runtime(image, simulation).unwrap();
        // Safety: all handles and callback storage remain live on this thread.
        unsafe {
            let x = vpi_handle_by_name(c"Top.x".as_ptr(), ptr::null_mut());
            assert!(!x.is_null());
            put_int(x, 0);
            assert!(!run_callbacks_result().unwrap());
            assert_eq!(read_int(c"Top.started"), 1);
            assert_eq!(read_int(c"Top.y"), 1);
            let mut previous = 0;
            for next in [7, 19] {
                let value = VpiValue {
                    format: VPI_INT_VAL,
                    value: VpiValueData { integer: next },
                };
                vpi_put_value(x, &value, ptr::null(), VPI_INERTIAL_DELAY);
                assert_eq!(read_int(c"Top.x"), previous);
                let mut observations = Vec::<(i32, i32)>::new();
                for reason in [CB_READ_WRITE_SYNCH, CB_READ_ONLY_SYNCH] {
                    let data = VpiCbData {
                        reason,
                        cb_rtn: Some(record_resumed_inertial_value),
                        obj: ptr::null_mut(),
                        time: ptr::null_mut(),
                        value: ptr::null_mut(),
                        index: 0,
                        user_data: ptr::from_mut(&mut observations).cast(),
                    };
                    assert!(!vpi_register_cb(&data).is_null());
                }
                assert!(!run_callbacks_result().unwrap());
                assert_eq!(celox_vpi_current_time(), 0);
                assert_eq!(read_int(c"Top.x"), next);
                assert_eq!(read_int(c"Top.y"), next + 1);
                assert_eq!(
                    observations,
                    vec![
                        (CB_READ_WRITE_SYNCH, next + 1),
                        (CB_READ_ONLY_SYNCH, next + 1)
                    ]
                );
                previous = next;
            }
            vpi_free_object(x);
        }
    }
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn record_resumed_inertial_value(data: *mut VpiCbData) -> i32 {
    // Safety: the test keeps the observation vector live until both callbacks fire.
    unsafe {
        let observations = &mut *(*data).user_data.cast::<Vec<(i32, i32)>>();
        observations.push(((*data).reason, read_int(c"Top.y")));
    }
    0
}
#[cfg(feature = "systemverilog")]
struct CallbackVectorValues {
    words: usize,
    values: Vec<Vec<VpiVecVal>>,
}

#[cfg(feature = "systemverilog")]
unsafe extern "C" fn record_callback_vector_values(data: *mut VpiCbData) -> i32 {
    // Safety: the test retains its context, and VPI supplies the requested vector.
    unsafe {
        let context = &mut *(*data).user_data.cast::<CallbackVectorValues>();
        let vector = (*(*data).value).value.vector;
        context
            .values
            .push(std::slice::from_raw_parts(vector, context.words).to_vec());
    }
    0
}

/// Change detection must observe high bits and changes in only the X/Z mask,
/// including transitions between values that do and do not fit one machine word.
#[cfg(feature = "systemverilog")]
#[test]
fn value_change_callbacks_compare_full_values_and_unknown_masks() {
    let _serial = lock_vpi();
    for four_state in [false, true] {
        for width in [63usize, 64, 65] {
            let source = format!(
                "module Top(input logic [{0}:0] x, output logic [{0}:0] y); assign y=x; endmodule",
                width - 1
            );
            let simulator = Simulator::from_sv_sources(
                vec![(&source, std::path::Path::new("callback_values.sv"))],
                "Top",
            )
            .four_state(four_state)
            .opt_level(celox::OptLevel::O1)
            .build()
            .unwrap();
            let _session = install_runtime(trusted_instance(
                simulator.shared_code().program_image().clone(),
            ))
            .unwrap();
            let zero = "0".repeat(width);
            let one = format!("{}1", "0".repeat(width - 1));
            let high_and_low = format!("1{}1", "0".repeat(width - 2));
            let unknown = format!("{}x", "0".repeat(width - 1));
            let high_z = format!("{}z", "0".repeat(width - 1));
            let mut observed = CallbackVectorValues {
                words: width.div_ceil(32),
                values: Vec::new(),
            };
            // Safety: handles, callback context and write buffers remain live.
            unsafe {
                let x = vpi_handle_by_name(c"Top.x".as_ptr(), ptr::null_mut());
                let y = vpi_handle_by_name(c"Top.y".as_ptr(), ptr::null_mut());
                assert!(!x.is_null() && !y.is_null());
                let initial = std::ffi::CString::new(zero.clone()).unwrap();
                let value = VpiValue {
                    format: VPI_BIN_STR_VAL,
                    value: VpiValueData {
                        str_: initial.as_ptr().cast_mut(),
                    },
                };
                assert_eq!(vpi_put_value(x, &value, ptr::null(), VPI_NO_DELAY), x);
                let mut value = VpiValue {
                    format: VPI_VECTOR_VAL,
                    value: VpiValueData {
                        vector: ptr::null_mut(),
                    },
                };
                let data = VpiCbData {
                    reason: CB_VALUE_CHANGE,
                    cb_rtn: Some(record_callback_vector_values),
                    obj: y,
                    time: ptr::null_mut(),
                    value: &mut value,
                    index: 0,
                    user_data: ptr::from_mut(&mut observed).cast(),
                };
                let callback = vpi_register_cb(&data);
                assert!(!callback.is_null());
                let mut previous = vec![VpiVecVal::default(); observed.words];
                let mut expected = Vec::new();
                for pattern in [
                    zero.clone(),
                    one.clone(),
                    one.clone(),
                    high_and_low,
                    one.clone(),
                    unknown,
                    one,
                    zero.clone(),
                    high_z,
                    zero.clone(),
                    "1".repeat(width),
                    "x".repeat(width),
                    "z".repeat(width),
                    zero,
                ] {
                    let mut vector = vec![VpiVecVal::default(); observed.words];
                    for (bit, digit) in pattern.bytes().rev().enumerate() {
                        let word = &mut vector[bit / 32];
                        let selected = 1u32 << (bit % 32);
                        if digit == b'1' || four_state && digit == b'x' {
                            word.aval |= selected as i32;
                        }
                        if four_state && matches!(digit, b'x' | b'z') {
                            word.bval |= selected as i32;
                        }
                    }
                    if vector != previous {
                        expected.push(vector.clone());
                        previous = vector;
                    }
                    let buffer = std::ffi::CString::new(pattern).unwrap();
                    let value = VpiValue {
                        format: VPI_BIN_STR_VAL,
                        value: VpiValueData {
                            str_: buffer.as_ptr().cast_mut(),
                        },
                    };
                    assert_eq!(vpi_put_value(x, &value, ptr::null(), VPI_NO_DELAY), x);
                    assert!(!run_callbacks_result().unwrap());
                    assert_eq!(
                        observed.values, expected,
                        "width={width}, four_state={four_state}"
                    );
                }
                assert_eq!(vpi_remove_cb(callback), 1);
                vpi_free_object(x);
                vpi_free_object(y);
            }
        }
    }
}

/// String and vector results have independent handle-owned scratch storage.
/// Exercise repeated formats, word boundaries and partial unknown hex nibbles.
#[cfg(feature = "systemverilog")]
#[test]
fn value_buffers_preserve_formats_and_independent_pointer_lifetimes() {
    let _serial = lock_vpi();
    let source = "module Top(input logic [2:0] p, input logic [64:0] w,
        input logic [128:0] v, output logic [2:0] po,
        output logic [64:0] wo, output logic [128:0] vo);
        assign po=p; assign wo=w; assign vo=v; endmodule";
    let simulator = Simulator::from_sv_sources(
        vec![(source, std::path::Path::new("value_buffers.sv"))],
        "Top",
    )
    .four_state(true)
    .opt_level(celox::OptLevel::O1)
    .build()
    .unwrap();
    let _session = install_runtime(trusted_instance(
        simulator.shared_code().program_image().clone(),
    ))
    .unwrap();
    let partial = [
        ("000", "0"),
        ("101", "5"),
        ("zzz", "z"),
        ("xxx", "x"),
        ("00z", "x"),
        ("x01", "x"),
    ]
    .into_iter()
    .map(|(b, h)| (b.to_string(), h.to_string()))
    .collect::<Vec<_>>();
    let mut cases = vec![(c"Top.p", 3usize, partial)];
    for (name, width) in [(c"Top.w", 65usize), (c"Top.v", 129usize)] {
        let digits = width.div_ceil(4);
        cases.push((
            name,
            width,
            vec![
                ("0".repeat(width), "0".repeat(digits)),
                (
                    format!("1{}", "0".repeat(width - 1)),
                    format!("1{}", "0".repeat(digits - 1)),
                ),
                (
                    format!("1{}1", "0".repeat(width - 2)),
                    format!("1{}1", "0".repeat(digits - 2)),
                ),
                (
                    format!("z{}", "0".repeat(width - 1)),
                    format!("z{}", "0".repeat(digits - 1)),
                ),
                ("x".repeat(width), "x".repeat(digits)),
                ("z".repeat(width), "z".repeat(digits)),
                ("0".repeat(width), "0".repeat(digits)),
            ],
        ));
    }
    // Safety: each handle is live, write strings remain live through deposit,
    // and pointers are checked only before their own scratch storage is replaced.
    unsafe {
        for (name, width, patterns) in cases {
            let handle = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
            assert!(!handle.is_null());
            for (binary, hex) in patterns {
                let input = std::ffi::CString::new(binary.clone()).unwrap();
                let deposit = VpiValue {
                    format: VPI_BIN_STR_VAL,
                    value: VpiValueData {
                        str_: input.as_ptr().cast_mut(),
                    },
                };
                assert_eq!(
                    vpi_put_value(handle, &deposit, ptr::null(), VPI_NO_DELAY),
                    handle
                );
                let mut read = VpiValue {
                    format: VPI_BIN_STR_VAL,
                    value: VpiValueData { integer: 0 },
                };
                vpi_get_value(handle, &mut read);
                let binary_pointer = read.value.str_;
                assert!(!binary_pointer.is_null());
                assert_eq!(CStr::from_ptr(binary_pointer).to_bytes(), binary.as_bytes());
                read.format = VPI_INT_VAL;
                vpi_get_value(handle, &mut read);
                let mut vector = VpiValue {
                    format: VPI_VECTOR_VAL,
                    value: VpiValueData {
                        vector: ptr::null_mut(),
                    },
                };
                vpi_get_value(handle, &mut vector);
                let vector_pointer = vector.value.vector;
                assert!(!vector_pointer.is_null());
                let words = width.div_ceil(32);
                let saved_vector = std::slice::from_raw_parts(vector_pointer, words).to_vec();
                assert_eq!(CStr::from_ptr(binary_pointer).to_bytes(), binary.as_bytes());
                read.format = VPI_HEX_STR_VAL;
                vpi_get_value(handle, &mut read);
                assert_eq!(CStr::from_ptr(read.value.str_).to_bytes(), hex.as_bytes());
                assert_eq!(
                    std::slice::from_raw_parts(vector_pointer, words),
                    saved_vector
                );
                read.format = VPI_BIN_STR_VAL;
                vpi_get_value(handle, &mut read);
                assert_eq!(
                    CStr::from_ptr(read.value.str_).to_bytes(),
                    binary.as_bytes()
                );
            }
            vpi_free_object(handle);
        }
    }
}

/// All writable formats must agree at narrow/wide boundaries, including
/// truncation, signed extension and two-state conversion of X/Z bits.
#[cfg(feature = "systemverilog")]
#[test]
fn deposits_preserve_formats_at_word_boundaries() {
    let _serial = lock_vpi();
    let widths = [1usize, 3, 8, 9, 31, 32, 33, 63, 64, 65, 96, 127, 128, 129];
    let ports = widths
        .iter()
        .flat_map(|width| {
            [
                format!("input logic [{}:0] p{width}", width - 1),
                format!("output logic [{}:0] q{width}", width - 1),
            ]
        })
        .collect::<Vec<_>>()
        .join(",");
    let assignments = widths
        .iter()
        .map(|width| format!("assign q{width}=p{width};"))
        .collect::<String>();
    let source = format!("module Top({ports}); {assignments} endmodule");
    for four_state in [false, true] {
        let simulator = Simulator::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("deposit_words.sv"))],
            "Top",
        )
        .four_state(four_state)
        .opt_level(celox::OptLevel::O1)
        .build()
        .unwrap();
        let _session = install_runtime(trusted_instance(
            simulator.shared_code().program_image().clone(),
        ))
        .unwrap();
        for width in widths {
            let name = std::ffi::CString::new(format!("Top.p{width}")).unwrap();
            // Safety: this session owns the live handle. Each format's input
            // storage remains readable through the synchronous deposit.
            unsafe {
                let handle = vpi_handle_by_name(name.as_ptr(), ptr::null_mut());
                assert!(!handle.is_null());
                let check = |value: &VpiValue, expected: &str| {
                    assert_eq!(
                        vpi_put_value(handle, value, ptr::null(), VPI_NO_DELAY),
                        handle
                    );
                    let mut read = VpiValue {
                        format: VPI_BIN_STR_VAL,
                        value: VpiValueData { integer: 0 },
                    };
                    vpi_get_value(handle, &mut read);
                    assert_eq!(
                        CStr::from_ptr(read.value.str_).to_str().unwrap(),
                        expected,
                        "width={width}, four_state={four_state}, format={}",
                        value.format
                    );
                };
                for integer in [0i32, -1, -2, 0x5a87_6543] {
                    let expected = (0..width)
                        .rev()
                        .map(|bit| {
                            let set = if bit >= 32 {
                                integer < 0
                            } else {
                                (integer as u32 >> bit) & 1 != 0
                            };
                            if set { '1' } else { '0' }
                        })
                        .collect::<String>();
                    check(
                        &VpiValue {
                            format: VPI_INT_VAL,
                            value: VpiValueData { integer },
                        },
                        &expected,
                    );
                }
                let vector = (0..width.div_ceil(32))
                    .map(|word| VpiVecVal {
                        aval: 0x96a5_3cf0u32.rotate_left(word as u32 * 7) as i32,
                        bval: 0xf30c_5a96u32.rotate_right(word as u32 * 5) as i32,
                    })
                    .collect::<Vec<_>>();
                let expected = (0..width)
                    .rev()
                    .map(|bit| {
                        let word = vector[bit / 32];
                        let a = ((word.aval as u32 >> (bit % 32)) & 1) != 0;
                        let b = ((word.bval as u32 >> (bit % 32)) & 1) != 0;
                        match (a, b, four_state) {
                            (_, true, false) => '0',
                            (true, true, true) => 'x',
                            (false, true, true) => 'z',
                            (true, false, _) => '1',
                            _ => '0',
                        }
                    })
                    .collect::<String>();
                check(
                    &VpiValue {
                        format: VPI_VECTOR_VAL,
                        value: VpiValueData {
                            vector: vector.as_ptr().cast_mut(),
                        },
                    },
                    &expected,
                );
                // The long strings exercise discarded prefixes as well as
                // partial high nibbles; mixed case and separators are valid.
                for (format, text, expanded) in [
                    (VPI_BIN_STR_VAL, "10_xZ01".repeat(40), "10xz01".repeat(40)),
                    (
                        VPI_HEX_STR_VAL,
                        "aF_xZ01".repeat(40),
                        "10101111xxxxzzzz00000001".repeat(40),
                    ),
                    (VPI_BIN_STR_VAL, "10101".repeat(40), "10101".repeat(40)),
                    (VPI_HEX_STR_VAL, "a5".repeat(40), "10100101".repeat(40)),
                ] {
                    let expected = expanded[expanded.len() - width..]
                        .chars()
                        .map(|c| {
                            if !four_state && matches!(c, 'x' | 'z') {
                                '0'
                            } else {
                                c
                            }
                        })
                        .collect::<String>();
                    let input = std::ffi::CString::new(text).unwrap();
                    check(
                        &VpiValue {
                            format,
                            value: VpiValueData {
                                str_: input.as_ptr().cast_mut(),
                            },
                        },
                        &expected,
                    );
                    for invalid_text in [
                        format!("!{}", "0".repeat(200)),
                        format!("+{}", "1".repeat(width)),
                    ] {
                        let invalid = std::ffi::CString::new(invalid_text).unwrap();
                        let value = VpiValue {
                            format,
                            value: VpiValueData {
                                str_: invalid.as_ptr().cast_mut(),
                            },
                        };
                        assert!(vpi_put_value(handle, &value, ptr::null(), VPI_NO_DELAY).is_null());
                        let mut read = VpiValue {
                            format: VPI_BIN_STR_VAL,
                            value: VpiValueData { integer: 0 },
                        };
                        vpi_get_value(handle, &mut read);
                        assert_eq!(CStr::from_ptr(read.value.str_).to_str().unwrap(), expected);
                    }
                }
                vpi_free_object(handle);
            }
        }
    }
}
