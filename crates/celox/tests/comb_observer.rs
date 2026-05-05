use celox::Simulator;

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
fn test_comb_display_follows_always_comb_sensitivity_after_settle(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<2>,
    unrelated: input logic<8>,
    y: output logic<8>,
) {
    always_comb {
        y = a & 2'd1;
        $display("y=%0d", y);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let unrelated = sim.signal("unrelated");
    let y = sim.signal("y");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "y=1".to_string(),
        }],
    );

    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(unrelated, 7u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 3u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "y=1".to_string(),
        }],
    );
}

fn test_comb_assert_continue_follows_always_comb_sensitivity_after_settle(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<2>,
    y: output logic<8>,
) {
    always_comb {
        y = a & 2'd1;
        $assert_continue(y != 8'd1, "bad y=%0d", y);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let y = sim.signal("y");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 0);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertContinue {
            message: "bad y=1".to_string(),
        }],
    );

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 3u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertContinue {
            message: "bad y=1".to_string(),
        }],
    );

    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 0);
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_keeps_static_unrolled_execution_count(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    sum: output logic<8>,
) {
    always_comb {
        sum = 8'd0;
        for i in 0..3 {
            sum = sum + a + i;
            $display("i=%0d sum=%0d", i, sum);
        }
    }
}
"#, "Top");

    let a = sim.signal("a");
    let sum = sim.signal("sum");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(sum), 9);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "i=0 sum=2".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1 sum=5".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=2 sum=9".to_string(),
            },
        ],
    );
}

fn test_comb_display_uses_statement_position_symbolic_values(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    seen: output logic<8>,
) {
    always_comb {
        seen = a;
        $display("first=%0d", seen);
        seen = a + 8'd1;
        $display("second=%0d", seen);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let seen = sim.signal("seen");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(seen), 2);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "first=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=2".to_string(),
            },
        ],
    );
}

fn test_comb_display_runtime_excludes_written_lhs_from_sensitivity(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    b: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        $display("before=%0d", tmp);
        tmp = a;
    }

    always_comb {
        out = tmp + b;
    }
}
"#, "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "before=0".to_string(),
        }],
    );

    sim.modify(|io| io.set(b, 4u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 5);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 6);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "before=1".to_string(),
        }],
    );
}

fn test_comb_display_snapshots_after_dynamic_bit_write_before_later_same_var_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    idx: input logic<3>,
    val: input logic,
    hi: input logic<4>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = 8'h00;
        tmp[idx] = val;
        $display("mid=%0d", tmp);
        tmp[7:4] = hi;
        out = tmp;
    }
}
"#, "Top");

    let idx = sim.signal("idx");
    let val = sim.signal("val");
    let hi = sim.signal("hi");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(idx, 2u8);
        io.set(val, 1u8);
        io.set(hi, 0xAu8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0xA4);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=4".to_string(),
        }],
    );

    sim.modify(|io| io.set(hi, 0xBu8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0xB4);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=4".to_string(),
        }],
    );
}

fn test_comb_display_snapshots_after_dynamic_array_write_before_later_same_array_write(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    idx: input logic<2>,
    val: input logic<8>,
    tail: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8> [4];

    always_comb {
        mem[0] = 8'd1;
        mem[1] = 8'd2;
        mem[2] = 8'd3;
        mem[3] = 8'd4;
        mem[idx] = val;
        $display("mid=%0d", mem[2]);
        mem[2] = tail;
        out = mem[2];
    }
}
"#, "Top");

    let idx = sim.signal("idx");
    let val = sim.signal("val");
    let tail = sim.signal("tail");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(idx, 2u8);
        io.set(val, 0x55u8);
        io.set(tail, 0xAAu8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0xAA);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=85".to_string(),
        }],
    );

    sim.modify(|io| io.set(tail, 0xBBu8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0xBB);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=85".to_string(),
        }],
    );
}

}

#[test]
fn test_comb_observer_sensitivity_excludes_written_lhs() {
    let sim = Simulator::builder(
        r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        $display("before=%0d", tmp);
        tmp = a;
        out = tmp;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let tmp_addr = sim.program().get_addr(&[], &["tmp"]).unwrap();
    let observer = &sim.program().comb_observers[0];
    assert!(
        observer.sensitivity.iter().all(|atom| atom.id != tmp_addr),
        "written LHS must be excluded from always_comb sensitivity: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_snapshot_order_can_target_statement_position_not_whole_var() {
    let result = Simulator::builder(
        r#"
module Top (
    a: input logic,
    hi: input logic,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp[0] = a;
        $display("mid=%0d", tmp[0]);
        tmp[7] = hi;
        out = tmp;
    }
}
"#,
        "Top",
    )
    .optimize(false)
    .trace_post_optimized_sir()
    .build_with_trace();
    let sir = result
        .trace
        .format_post_optimized_sir()
        .expect("post-optimized SIR should be traced");

    let store_low = sir
        .find("Store(addr=tmp (region=0), offset=0, bits=1")
        .unwrap_or_else(|| panic!("missing low-bit store in SIR:\n{sir}"));
    let store_observer = sir
        .find("StoreObserver(")
        .unwrap_or_else(|| panic!("missing observer store in SIR:\n{sir}"));
    let store_high = sir
        .find("Store(addr=tmp (region=0), offset=7, bits=1")
        .unwrap_or_else(|| panic!("missing high-bit store in SIR:\n{sir}"));

    assert!(
        store_low < store_observer && store_observer < store_high,
        "observer snapshot should be ordered after the prior low-bit write and before the unrelated later high-bit write:\n{sir}"
    );
}

#[test]
fn test_comb_observer_snapshot_order_can_place_multiple_observers_on_same_var() {
    let result = Simulator::builder(
        r#"
module Top (
    a: input logic,
    b: input logic,
    hi: input logic,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp[0] = a;
        $display("first=%0d", tmp[0]);
        tmp[1] = b;
        $display("second=%0d", tmp[1]);
        tmp[7] = hi;
        out = tmp;
    }
}
"#,
        "Top",
    )
    .optimize(false)
    .trace_post_optimized_sir()
    .build_with_trace();
    let sir = result
        .trace
        .format_post_optimized_sir()
        .expect("post-optimized SIR should be traced");

    let store_bit0 = sir
        .find("Store(addr=tmp (region=0), offset=0, bits=1")
        .unwrap_or_else(|| panic!("missing bit 0 store in SIR:\n{sir}"));
    let first_observer = sir
        .find("StoreObserver(obs0")
        .unwrap_or_else(|| panic!("missing first observer store in SIR:\n{sir}"));
    let store_bit1 = sir
        .find("Store(addr=tmp (region=0), offset=1, bits=1")
        .unwrap_or_else(|| panic!("missing bit 1 store in SIR:\n{sir}"));
    let second_observer = sir
        .find("StoreObserver(obs1")
        .unwrap_or_else(|| panic!("missing second observer store in SIR:\n{sir}"));
    let store_bit7 = sir
        .find("Store(addr=tmp (region=0), offset=7, bits=1")
        .unwrap_or_else(|| panic!("missing bit 7 store in SIR:\n{sir}"));

    assert!(
        store_bit0 < first_observer
            && first_observer < store_bit1
            && store_bit1 < second_observer
            && second_observer < store_bit7,
        "each observer snapshot should be placeable at its own statement position on the same variable:\n{sir}"
    );
}
