use std::{
    ffi::CStr,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
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
