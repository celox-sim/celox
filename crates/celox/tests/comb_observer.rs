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
