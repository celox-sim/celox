use celox::{DeadStorePolicy, Simulator};

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

fn test_comb_display_survives_dead_store_elimination(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    unrelated: input logic<8>,
    out: output logic,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = a;
    }

    always_comb {
        out = tmp[0];
        $display("tmp=%0d", tmp);
    }
}
"#, "Top").dead_store_policy(DeadStorePolicy::PreserveTopPorts);

    let a = sim.signal("a");
    let unrelated = sim.signal("unrelated");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "tmp=5".to_string(),
        }],
    );

    sim.modify(|io| io.set(unrelated, 7u8)).unwrap();
    assert_eq!(sim.drain_runtime_events(), Vec::new());
}

fn test_comb_constant_display_runs_on_initial_eval_only(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
) {
    always_comb {
        $display("hi");
    }
}
"#, "Top");

    let a = sim.signal("a");

    // IEEE 1800-2023 9.2.2.2.2 requires always_comb to execute once at time
    // zero. That process execution is distinct from the implicit sensitivity
    // in 9.2.2.2.1, so a constant observer still runs once even though it has
    // no sensitivity inputs.
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "hi".to_string(),
        }],
    );

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.drain_runtime_events(), Vec::new());
}

fn test_comb_constant_fatal_assert_runs_on_initial_eval(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top {
    always_comb {
        $assert(1'd0, "constant fail");
    }
}
"#, "Top");

    // IEEE 1800-2023 9.2.2.2.2 time-zero execution applies to assertions too:
    // a constant fatal assert in always_comb must terminate the initial comb
    // evaluation instead of waiting for a sensitivity change that cannot occur.
    let err = sim.eval_comb().unwrap_err();
    assert_eq!(
        err,
        celox::RuntimeErrorCode::Runtime {
            message: "constant fail".to_string(),
            signals: Vec::new(),
        },
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertFatal {
            message: "constant fail".to_string(),
        }],
    );
}

fn test_comb_sensitive_display_runs_on_initial_eval(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
) {
    always_comb {
        $display("a=%0d", a);
    }
}
"#, "Top");

    // IEEE 1800-2023 9.2.2.2.2 requires always_comb execution at time zero
    // even when 9.2.2.2.1 gives the process a non-empty implicit sensitivity
    // list and the sensitized values have not changed from their defaults.
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=0".to_string(),
        }],
    );
}

fn test_comb_sensitive_fatal_assert_runs_on_initial_eval(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
) {
    always_comb {
        $assert(a, "a must be set");
    }
}
"#, "Top");

    let err = sim.eval_comb().unwrap_err();
    assert_eq!(
        err,
        celox::RuntimeErrorCode::Runtime {
            message: "a must be set".to_string(),
            signals: Vec::new(),
        },
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertFatal {
            message: "a must be set".to_string(),
        }],
    );
}

fn test_comb_runtime_event_drain_settles_dirty_comb_before_reading_events(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        out = a;
        $display("a=%0d", a);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");
    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 7u8)).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=7".to_string(),
        }],
    );
    assert_eq!(sim.get_as::<u8>(out), 7);
}

fn test_comb_runtime_event_drain_handle_sees_captured_display(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        out = a;
        $display("a=%0d", a);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");
    sim.drain_runtime_events();
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");

    sim.modify(|io| io.set(a, 11u8)).unwrap();
    assert_eq!(
        drain.drain(),
        vec![celox::RuntimeEvent::Display {
            message: "a=11".to_string(),
        }],
    );
    assert_eq!(sim.get_as::<u8>(out), 11);
    assert_eq!(drain.drain(), Vec::new());
}

fn test_comb_runtime_event_drain_handle_sees_direct_set_capture(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        out = a;
        $display("a=%0d", a);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");
    sim.drain_runtime_events();
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");

    sim.set(a, 12u8);
    assert_eq!(
        drain.drain(),
        vec![celox::RuntimeEvent::Display {
            message: "a=12".to_string(),
        }],
    );
    assert_eq!(sim.get_as::<u8>(out), 12);
}

fn test_comb_runtime_event_drain_handle_starts_after_simulator_drain(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        out = a;
        $display("a=%0d", a);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");
    sim.eval_comb().unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=0".to_string(),
        }],
    );
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");
    assert_eq!(drain.drain(), Vec::new());

    sim.modify(|io| io.set(a, 13u8)).unwrap();
    assert_eq!(
        drain.drain(),
        vec![celox::RuntimeEvent::Display {
            message: "a=13".to_string(),
        }],
    );
    assert_eq!(sim.get_as::<u8>(out), 13);
}

fn test_comb_runtime_event_drain_handle_preserves_ff_before_comb_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    out: output logic<8>,
) {
    var q: logic<8>;

    always_ff {
        q = d;
        $display("ff=%0d", d);
    }

    always_comb {
        out = q;
        $display("comb=%0d", out);
    }
}
"#, "Top");

    let clk = sim.event("clk");
    let d = sim.signal("d");
    let out = sim.signal("out");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");

    sim.modify(|io| io.set(d, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 3);
    assert_eq!(
        drain.drain(),
        vec![
            celox::RuntimeEvent::Display {
                message: "ff=3".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "comb=3".to_string(),
            },
        ],
    );
}

fn test_comb_runtime_event_drain_handle_drains_older_comb_before_later_ff(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input logic<8>,
    d: input logic<8>,
    out: output logic<8>,
) {
    always_ff {
        $display("ff=%0d", d);
    }

    always_comb {
        out = a;
        $display("comb=%0d", a);
    }
}
"#, "Top");

    let clk = sim.event("clk");
    let a = sim.signal("a");
    let d = sim.signal("d");
    let out = sim.signal("out");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();
    let mut drain = sim.runtime_event_drain().expect("runtime event drain handle");

    sim.modify(|io| io.set(a, 9u8)).unwrap();
    sim.modify(|io| io.set(d, 4u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 9);
    assert_eq!(
        drain.drain(),
        vec![
            celox::RuntimeEvent::Display {
                message: "comb=9".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "ff=4".to_string(),
            },
        ],
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

fn test_comb_assert_fatal_stops_comb_eval_and_keeps_event(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    y: output logic<8>,
) {
    always_comb {
        y = a;
        $assert(y != 8'd1, "fatal y");
        $display("after fatal");
    }
}
"#, "Top");

    let a = sim.signal("a");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    let err = sim.eval_comb().unwrap_err();
    assert_eq!(
        err,
        celox::RuntimeErrorCode::Runtime {
            message: "fatal y".to_string(),
            signals: Vec::new(),
        },
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertFatal {
            message: "fatal y".to_string(),
        }],
    );
}

fn test_comb_assert_fatal_inactive_site_does_not_error(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    unrelated: input logic<8>,
    y: output logic<8>,
    z: output logic<8>,
) {
    always_comb {
        y = a;
        $assert(y != 8'd1, "fatal y");
    }

    always_comb {
        z = unrelated;
    }
}
"#, "Top");

    let a = sim.signal("a");
    let unrelated = sim.signal("unrelated");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert!(sim.eval_comb().is_err());
    sim.drain_runtime_events();

    sim.modify(|io| io.set(unrelated, 9u8)).unwrap();
    sim.eval_comb().unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_pending_events_drain_before_later_ff_events(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    clk: input clock,
    a: input logic<8>,
    d: input logic<8>,
    y: output logic<8>,
) {
    always_ff {
        $display("ff=%0d", d);
    }

    always_comb {
        y = a;
        $display("comb=%0d", y);
    }
}
"#, "Top");

    let clk = sim.event("clk");
    let a = sim.signal("a");
    let d = sim.signal("d");
    let y = sim.signal("y");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 5);
    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "comb=5".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "ff=7".to_string(),
            },
        ],
    );
}

fn test_comb_display_ff_triggered_comb_only_captures_active_sites(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    a: input logic<8>,
    y: output logic<8>,
    z: output logic<8>,
) {
    var q: logic<8>;

    always_ff {
        q = d;
    }

    always_comb {
        y = q;
        $display("q=%0d", q);
    }

    always_comb {
        z = a;
        for i in 0..1100 {
            $display("inactive=%0d", a);
        }
    }
}
"#, "Top");

    let clk = sim.event("clk");
    let d = sim.signal("d");
    let y = sim.signal("y");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(d, 7u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 7);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "q=7".to_string(),
        }],
    );

    sim.tick(clk).unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_tracks_downstream_comb_settle_sensitivity(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var x: logic<8>;

    always_comb {
        x = a;
    }

    always_comb {
        out = x;
        $display("x=%0d", x);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 9u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 9);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "x=9".to_string(),
        }],
    );
}

fn test_comb_display_coalesces_sensitive_changes_before_observer_executes(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    src: input logic<8>,
    out_a: output logic<8>,
    out_b: output logic<8>,
) {
    var a: logic<8>;
    var b: logic<8>;

    always_comb {
        a = src;
    }

    always_comb {
        b = a;
    }

    always_comb {
        out_a = a;
        out_b = b;
        $display("a=%0d b=%0d", a, b);
    }
}
"#, "Top");

    let src = sim.signal("src");
    let out_a = sim.signal("out_a");
    let out_b = sim.signal("out_b");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(src, 5u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out_a), 5);
    assert_eq!(sim.get_as::<u8>(out_b), 5);

    // IEEE 1800-2023 9.2.2.2.1 makes this observer sensitive to both a and b,
    // while 4.7 leaves Active event ordering nondeterministic. Celox is allowed
    // to coalesce multiple sensitivity changes into one pending process
    // execution and emit the side effect once at the settled observation point.
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=5 b=5".to_string(),
        }],
    );
}

fn test_comb_display_inside_writer_reactivates_after_assign_chain(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    o: output logic,
) {
    var x: logic<2>;
    var y: logic;

    always_comb {
        x[0] = a;
        $display("x1=%0d", x[1]);
        o = x[1];
    }

    assign y = x[0];
    assign x[1] = y;
}
"#, "Top");

    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(o), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "x1=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "x1=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_writer_reactivates_after_multi_stage_assign_chain(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    o: output logic,
) {
    var x: logic<4>;

    always_comb {
        x[0] = a;
        $display("x3=%0d", x[3]);
        o = x[3];
    }

    assign x[1] = x[0];
    assign x[2] = x[1];
    assign x[3] = x[2];
}
"#, "Top");

    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(o), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "x3=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "x3=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_writer_preserves_ordered_downstream_reactivations(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    out: output logic<2>,
) {
    var x: logic<3>;

    always_comb {
        x[0] = a;
        $display("x1=%0d x2=%0d", x[1], x[2]);
        out = {x[2], x[1]};
    }

    assign x[1] = x[0];
    assign x[2] = x[1];
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 3);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "x1=0 x2=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "x1=1 x2=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "x1=1 x2=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_writer_reactivates_through_dynamic_index_read(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    i: input logic<2>,
    j: input logic<2>,
    o: output logic,
) {
    var w: logic<4>;
    var x: logic<2>;

    always_comb {
        w = 4'd0;
        w[i] = a;
        $display("xj=%0d", x[j]);
        o = x[j];
    }

    assign x[0] = w[0];
    assign x[1] = x[0];
}
"#, "Top");

    let a = sim.signal("a");
    let i = sim.signal("i");
    let j = sim.signal("j");
    let o = sim.signal("o");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(i, 0u8);
        io.set(j, 1u8);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(o), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "xj=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "xj=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "xj=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_guard_reactivates_after_assign_chain_changes_guard(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    o: output logic,
) {
    var x: logic<2>;

    always_comb {
        x[0] = a;
        if x[1] {
            $display("hit x1=%0d", x[1]);
        }
        o = x[1];
    }

    assign x[1] = x[0];
}
"#, "Top");

    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(o), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "hit x1=1".to_string(),
        }],
    );
}

fn test_comb_assert_fatal_reactivates_after_assign_chain_changes_assert_input(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    o: output logic,
) {
    var x: logic<2>;

    always_comb {
        x[0] = a;
        $assert(x[1] == 1'd0, "x1 became %0d", x[1]);
        o = x[1];
    }

    assign x[1] = x[0];
}
"#, "Top");

    let a = sim.signal("a");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    let err = sim.eval_comb().unwrap_err();
    assert_eq!(
        err,
        celox::RuntimeErrorCode::Runtime {
            message: "x1 became 1".to_string(),
            signals: Vec::new(),
        },
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertFatal {
            message: "x1 became 1".to_string(),
        }],
    );
}

fn test_comb_multiple_observers_inside_writer_reactivate_in_statement_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    b: input logic,
    out: output logic<2>,
) {
    var x: logic<4>;

    always_comb {
        x[0] = a;
        $display("first=%0d", x[1]);
        x[2] = b;
        $display("second=%0d", x[3]);
        out = {x[3], x[1]};
    }

    assign x[1] = x[0];
    assign x[3] = x[2];
}
"#, "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(a, 1u8);
        io.set(b, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 3);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "first=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "first=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_dynamic_for_reactivates_after_assign_chain(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic,
    count: input logic<3>,
    o: output logic,
) {
    var seed: logic;
    var x0: logic;
    var x1: logic;

    always_comb {
        seed = a;
        for i in 0..count {
            $display("i=%0d x1=%0d", i, x1);
        }
        o = x1;
    }

    assign x0 = seed;
    assign x1 = x0;
}
"#, "Top");

    let a = sim.signal("a");
    let count = sim.signal("count");
    let o = sim.signal("o");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(count, 2u8);
        io.set(a, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(o), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "i=0 x1=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1 x1=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=0 x1=1".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1 x1=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_writer_reactivates_through_instance_port_chain(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Passthrough (
    i: input logic,
    o: output logic,
) {
    assign o = i;
}

module Top (
    a: input logic,
    o: output logic,
) {
    var x: logic<2>;
    var mid: logic;

    inst u: Passthrough (
        i: x[0],
        o: mid,
    );

    always_comb {
        x[0] = a;
        $display("x1=%0d", x[1]);
        o = x[1];
    }

    assign x[1] = mid;
}
"#, "Top");

    let a = sim.signal("a");
    let o = sim.signal("o");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(o), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "x1=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "x1=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_after_ff_runtime_event_preserves_drain_order(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    out: output logic<8>,
) {
    var q: logic<8>;

    always_ff {
        q = d;
        $display("ff=%0d", d);
    }

    always_comb {
        out = q;
        $display("comb=%0d", out);
    }
}
"#, "Top");

    let clk = sim.event("clk");
    let d = sim.signal("d");
    let out = sim.signal("out");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(d, 3u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 3);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "ff=3".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "comb=3".to_string(),
            },
        ],
    );
}

fn test_comb_display_capture_defers_context_formatting_until_drain(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        out = a;
        $display("loc=%m time=%t a=%0d", a);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 5);
    assert_eq!(
        sim.drain_runtime_events_with_context(celox::RuntimeFormatContext {
            tb_time: Some(42),
            scope: Some("tb.top"),
        }),
        vec![celox::RuntimeEvent::Display {
            message: "loc=tb.top time=42 a=5".to_string(),
        }],
    );
}

fn test_comb_inactive_display_and_assert_do_not_leak_or_duplicate(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    b: input logic<8>,
    y: output logic<8>,
    z: output logic<8>,
) {
    always_comb {
        y = a;
        $display("active=%0d", y);
    }

    always_comb {
        z = b;
        $display("inactive_display=%0d", z);
        $assert_continue(z == 8'd255, "inactive_assert=%0d", z);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let b = sim.signal("b");
    let y = sim.signal("y");
    let z = sim.signal("z");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 3u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 3);
    assert_eq!(sim.get_as::<u8>(z), 0);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "active=3".to_string(),
        }],
    );

    sim.modify(|io| io.set(a, 4u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(y), 4);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "active=4".to_string(),
        }],
    );

    sim.modify(|io| io.set(b, 7u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(z), 7);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "inactive_display=7".to_string(),
            },
            celox::RuntimeEvent::AssertContinue {
                message: "inactive_assert=7".to_string(),
            },
        ],
    );
}

fn test_comb_multiple_active_captures_skip_inactive_site_between_evals(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    b: input logic<8>,
    out: output logic<8>,
    inactive_out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = a + 8'd1;
        $display("first=%0d", tmp);
        tmp = tmp + 8'd1;
        $display("second=%0d", tmp);
        out = tmp;
    }

    always_comb {
        inactive_out = b;
        $display("inactive=%0d", inactive_out);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");
    let inactive_out = sim.signal("inactive_out");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 5u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 7);
    assert_eq!(sim.get_as::<u8>(inactive_out), 0);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "first=6".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=7".to_string(),
            },
        ],
    );

    sim.modify(|io| io.set(a, 6u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 8);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "first=7".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=8".to_string(),
            },
        ],
    );
}

fn test_comb_capture_before_dynamic_for_backedge_keeps_loop_state(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    out: output logic<8>,
) {
    var sum: logic<8>;

    always_comb {
        sum = base;
        for i in 0..count {
            $display("before i=%0d sum=%0d", i, sum);
            sum = sum + i + 8'd1;
        }
        out = sum;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let base = sim.signal("base");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(count, 3u8);
        io.set(base, 10u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 16);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "before i=0 sum=10".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "before i=1 sum=11".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "before i=2 sum=13".to_string(),
            },
        ],
    );
}

fn test_comb_capture_after_branch_preserves_phi_args(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    sel: input logic,
    a: input logic<8>,
    b: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        if sel {
            tmp = a;
            $display("then=%0d", tmp);
        } else {
            tmp = b;
            $display("else=%0d", tmp);
        }
        out = tmp + 8'd1;
    }
}
"#, "Top");

    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(a, 20u8);
        io.set(b, 40u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 21);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "then=20".to_string(),
        }],
    );

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 41);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "else=40".to_string(),
        }],
    );
}

fn test_comb_capture_preserves_wide_four_state_args(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<80>,
    out: output logic<80>,
) {
    always_comb {
        out = a;
        $display("a=%b", a);
    }
}
"#, "Top").four_state(true);

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set_four_state(
            a,
            num_bigint::BigUint::from(0x1234_5678_9abc_def0u64),
            num_bigint::BigUint::from(0x0000_0000_0000_00f0u64),
        )
    })
    .unwrap();
    assert_eq!(sim.get_four_state(out).0, num_bigint::BigUint::from(0x1234_5678_9abc_def0u64));
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=000000000000000000010010001101000101011001111000100110101011110011011110xxxx0000"
                .to_string(),
        }],
    );
}

fn test_comb_display_inside_statement_function_call(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    function bump (
        x: input logic<8>,
        y: output logic<8>,
    ) {
        y = x + 8'd1;
        $display("bump=%0d", y);
    }

    always_comb {
        bump(a, out);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 9u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 10);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "bump=10".to_string(),
        }],
    );
}

fn test_comb_display_inside_expression_function_call(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    function twice (
        x: input logic<8>,
    ) -> logic<8> {
        $display("arg=%0d", x);
        return x * 8'd2;
    }

    always_comb {
        out = twice(a) + 8'd1;
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 6u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 13);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "arg=6".to_string(),
        }],
    );
}

fn test_comb_assert_inside_function_call_uses_caller_sensitivity(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    unrelated: input logic<8>,
    out: output logic<8>,
) {
    function check (
        x: input logic<8>,
    ) -> logic<8> {
        $assert_continue(x != 8'd0, "zero=%0d", x);
        return x;
    }

    always_comb {
        out = check(a);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let unrelated = sim.signal("unrelated");
    let out = sim.signal("out");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(unrelated, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 1);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(a, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::AssertContinue {
            message: "zero=0".to_string(),
        }],
    );
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

fn test_comb_display_snapshots_between_dynamic_writes_to_same_var(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    base: input logic<8>,
    i: input logic<3>,
    x: input logic,
    j: input logic<3>,
    k: input logic<3>,
    y: input logic,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = base;
        tmp[i] = x;
        $display("mid=%0d", tmp[j]);
        tmp[k] = y;
        out = tmp[j];
    }
}
"#, "Top");

    let base = sim.signal("base");
    let i = sim.signal("i");
    let x = sim.signal("x");
    let j = sim.signal("j");
    let k = sim.signal("k");
    let y = sim.signal("y");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(base, 0u8);
        io.set(i, 2u8);
        io.set(x, 1u8);
        io.set(j, 2u8);
        io.set(k, 2u8);
        io.set(y, 0u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=1".to_string(),
        }],
    );

    sim.modify(|io| io.set(y, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=1".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set(j, 3u8);
        io.set(k, 3u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 1);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=0".to_string(),
        }],
    );
}

fn test_comb_display_snapshots_repeated_full_var_writes(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = a;
        $display("first=%0d", tmp);
        tmp = a + 8'd1;
        $display("second=%0d", tmp);
        tmp = a + 8'd2;
        $display("third=%0d", tmp);
        out = tmp;
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 3u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 5);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "first=3".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "second=4".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "third=5".to_string(),
            },
        ],
    );
}

fn test_comb_display_snapshots_inside_if_branch(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    en: input logic,
    a: input logic<8>,
    b: input logic<8>,
    c: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        if en {
            tmp = a;
            $display("then=%0d", tmp);
        } else {
            tmp = b;
            $display("else=%0d", tmp);
        }
        tmp = c;
        out = tmp;
    }
}
"#, "Top");

    let en = sim.signal("en");
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(en, 1u8);
        io.set(a, 7u8);
        io.set(b, 11u8);
        io.set(c, 19u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 19);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "then=7".to_string(),
        }],
    );

    sim.modify(|io| io.set(en, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 19);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "else=11".to_string(),
        }],
    );
}

fn test_comb_display_snapshots_repeated_writes_in_unrolled_loop(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = 8'd0;
        for i in 0..3 {
            tmp = tmp + a + i;
            $display("i=%0d tmp=%0d", i, tmp);
        }
        out = tmp;
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 9);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "i=0 tmp=2".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1 tmp=5".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=2 tmp=9".to_string(),
            },
        ],
    );
}

fn test_comb_display_snapshots_after_function_output_argument(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    tail: input logic<8>,
    out: output logic<8>,
) {
    function f (
        x: input logic<8>,
        y: output logic<8>,
    ) {
        y = x + 8'd3;
    }

    var tmp: logic<8>;

    always_comb {
        f(a, tmp);
        $display("after_f=%0d", tmp);
        tmp = tail;
        out = tmp;
    }
}
"#, "Top");

    let a = sim.signal("a");
    let tail = sim.signal("tail");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(tail, 99u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 99);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "after_f=13".to_string(),
        }],
    );
}

fn test_comb_display_snapshots_partial_overlap_position(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    lo: input logic<4>,
    hi: input logic<4>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = 8'h80;
        tmp[3:0] = lo;
        $display("mid=%0d", tmp);
        tmp[7:4] = hi;
        out = tmp;
    }
}
"#, "Top");

    let lo = sim.signal("lo");
    let hi = sim.signal("hi");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(lo, 0x5u8);
        io.set(hi, 0xAu8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0xA5);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "mid=133".to_string(),
        }],
    );
}

fn test_comb_display_snapshots_after_dynamic_for(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    out: output logic<8>,
) {
    var sum: logic<8>;

    always_comb {
        sum = base;
        for i in 0..count {
            sum = sum + i;
        }
        $display("sum=%0d", sum);
        out = sum;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let base = sim.signal("base");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(count, 4u8);
        io.set(base, 10u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 16);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "sum=16".to_string(),
        }],
    );
}

fn test_comb_display_inside_dynamic_for_runs_each_iteration(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    out: output logic<8>,
) {
    var sum: logic<8>;

    always_comb {
        sum = base;
        for i in 0..count {
            sum = sum + i;
            $display("i=%0d sum=%0d", i, sum);
        }
        out = sum;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let base = sim.signal("base");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(count, 4u8);
        io.set(base, 10u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 16);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "i=0 sum=10".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1 sum=11".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=2 sum=13".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=3 sum=16".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_dynamic_for_remaps_site_after_prior_comb_event(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    marker: input logic<8>,
    out: output logic<8>,
) {
    var sum: logic<8>;

    always_comb {
        $display("first=%0d", marker);
    }

    always_comb {
        sum = base;
        for i in 0..count {
            sum = sum + i;
            $display("i=%0d sum=%0d", i, sum);
        }
        out = sum;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let base = sim.signal("base");
    let out = sim.signal("out");

    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "first=0".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set(count, 3u8);
        io.set(base, 10u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 13);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "i=0 sum=10".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1 sum=11".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=2 sum=13".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_dynamic_for_preserves_repeated_identical_events(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    out: output logic<8>,
) {
    always_comb {
        for i in 0..count {
            $display("same");
        }
        out = count;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(count, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 2);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "same".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "same".to_string(),
            },
        ],
    );
}

fn test_comb_display_inside_dynamic_for_with_multiple_updates_emits_once_per_iteration(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    out_a: output logic<8>,
    out_b: output logic<8>,
) {
    var a: logic<8>;
    var b: logic<8>;

    always_comb {
        a = base;
        b = base + 8'd10;
        for i in 0..count {
            a = a + i;
            b = b + i;
            $display("i=%0d", i);
        }
        out_a = a;
        out_b = b;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let base = sim.signal("base");
    let out_a = sim.signal("out_a");
    let out_b = sim.signal("out_b");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(count, 2u8);
        io.set(base, 5u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out_a), 6);
    assert_eq!(sim.get_as::<u8>(out_b), 16);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "i=0".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "i=1".to_string(),
            },
        ],
    );
}

fn test_comb_display_preserves_order_around_dynamic_for(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    out: output logic<8>,
) {
    var sum: logic<8>;

    always_comb {
        sum = base;
        $display("before=%0d", sum);
        for i in 0..count {
            sum = sum + i;
            $display("inside i=%0d sum=%0d", i, sum);
        }
        $display("after=%0d", sum);
        out = sum;
    }
}
"#, "Top");

    let count = sim.signal("count");
    let base = sim.signal("base");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(count, 3u8);
        io.set(base, 10u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 13);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![
            celox::RuntimeEvent::Display {
                message: "before=10".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "inside i=0 sum=10".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "inside i=1 sum=11".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "inside i=2 sum=13".to_string(),
            },
            celox::RuntimeEvent::Display {
                message: "after=13".to_string(),
            },
        ],
    );
}

fn test_comb_display_downstream_wide_store_enables_observer(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<128>,
    unrelated: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<128>;

    always_comb {
        tmp = a;
    }

    always_comb {
        out = tmp[7:0];
        $display("lo=%0d hi=%0d", tmp[7:0], tmp[71:64]);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let unrelated = sim.signal("unrelated");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    let value: num_bigint::BigUint = (num_bigint::BigUint::from(0x12u8) << 64)
        | num_bigint::BigUint::from(0x34u8);
    sim.modify(|io| io.set_wide(a, value.clone())).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0x34);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "lo=52 hi=18".to_string(),
        }],
    );

    sim.modify(|io| io.set_wide(a, value)).unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(unrelated, 1u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0x34);
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_downstream_dynamic_write_crossing_word_boundary(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    idx: input logic<7>,
    val: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<128>;

    always_comb {
        tmp = 128'd0;
        tmp[idx +: 8] = val;
    }

    always_comb {
        out = tmp[67:60];
        $display("slice=%0d", tmp[67:60]);
    }
}
"#, "Top");

    let idx = sim.signal("idx");
    let val = sim.signal("val");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(idx, 60u8);
        io.set(val, 0xABu8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0xAB);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "slice=171".to_string(),
        }],
    );

    sim.modify(|io| io.set(val, 0xABu8)).unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_store_coalesce_does_not_enable_unrelated_chunk(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    lo: input logic<8>,
    hi: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<16>;

    always_comb {
        tmp[7:0] = lo;
        tmp[15:8] = hi;
    }

    always_comb {
        out = tmp[7:0];
        $display("lo=%0d", tmp[7:0]);
    }
}
"#, "Top");

    let lo = sim.signal("lo");
    let hi = sim.signal("hi");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(lo, 5u8);
        io.set(hi, 1u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 5);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "lo=5".to_string(),
        }],
    );

    sim.modify(|io| io.set(hi, 2u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 5);
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_ff_to_downstream_comb_store_enables_observer(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    clk: input clock,
    d: input logic<8>,
    out: output logic<8>,
) {
    var q: logic<8>;
    var x: logic<8>;

    always_ff {
        q = d;
    }

    always_comb {
        x = q;
    }

    always_comb {
        out = x;
        $display("x=%0d", x);
    }
}
"#, "Top");

    let clk = sim.event("clk");
    let d = sim.signal("d");
    let out = sim.signal("out");

    sim.eval_comb().unwrap();
    sim.drain_runtime_events();

    sim.modify(|io| io.set(d, 11u8)).unwrap();
    sim.tick(clk).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 11);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "x=11".to_string(),
        }],
    );

    sim.tick(clk).unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_port_alias_write_enables_downstream_observer(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Child (
    a: input logic<8>,
    y: output logic<8>,
) {
    always_comb {
        y = a;
    }
}

module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var internal: logic<8>;

    inst child: Child (
        a: a,
        y: internal,
    );

    always_comb {
        out = internal;
        $display("internal=%0d", internal);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 21u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 21);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "internal=21".to_string(),
        }],
    );

    sim.modify(|io| io.set(a, 21u8)).unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);
}

fn test_comb_display_four_state_mask_only_input_change_triggers(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    always_comb {
        out = a;
        $display("a=%b", a);
    }
}
"#, "Top").four_state(true);

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set_four_state(
            a,
            num_bigint::BigUint::from(0xA0u8),
            num_bigint::BigUint::from(0u8),
        )
    })
    .unwrap();
    assert_eq!(sim.get_four_state(out).0, num_bigint::BigUint::from(0xA0u8));
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=10100000".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set_four_state(
            a,
            num_bigint::BigUint::from(0xA0u8),
            num_bigint::BigUint::from(0x0Fu8),
        )
    })
    .unwrap();
    assert_eq!(
        sim.get_four_state(out).1,
        num_bigint::BigUint::from(0x0Fu8)
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "a=1010xxxx".to_string(),
        }],
    );
}

fn test_comb_display_unaligned_wide_store_enables_downstream_observer(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<127>,
    out: output logic<8>,
) {
    var tmp: logic<128>;

    always_comb {
        tmp = 128'd0;
        tmp[127:1] = a;
    }

    always_comb {
        out = tmp[8:1];
        $display("slice=%0d", tmp[8:1]);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set_wide(a, num_bigint::BigUint::from(0x5Au8)))
        .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0x5A);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "slice=90".to_string(),
        }],
    );
}

fn test_comb_display_wide_four_state_mask_store_enables_downstream_observer(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<80>,
    out: output logic<8>,
) {
    var tmp: logic<80>;

    always_comb {
        tmp = a;
    }

    always_comb {
        out = tmp[7:0];
        $display("lo=%b", tmp[7:0]);
    }
}
"#, "Top").four_state(true);

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set_four_state(
            a,
            num_bigint::BigUint::from(0x10u8),
            num_bigint::BigUint::from(0u8),
        )
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0x10);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "lo=00010000".to_string(),
        }],
    );

    sim.modify(|io| {
        io.set_four_state(
            a,
            num_bigint::BigUint::from(0x10u8),
            num_bigint::BigUint::from(0x0Fu8),
        )
    })
    .unwrap();
    assert_eq!(
        sim.get_four_state(out).1,
        num_bigint::BigUint::from(0x0Fu8)
    );
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "lo=0001xxxx".to_string(),
        }],
    );
}

fn test_comb_display_function_output_dynamic_actual_excludes_only_prefix(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    idx: input logic,
    a: input logic,
    b: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    function poke (
        value: input logic,
        dst: output logic,
    ) {
        dst = value;
    }

    always_comb {
        mem[2] = b;
    }

    always_comb {
        $display("v=%0d", mem[2]);
        poke(a, mem[1][idx]);
        out = mem[2];
    }
}
"#, "Top");

    let b = sim.signal("b");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(b, 0x33u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0x33);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "v=51".to_string(),
        }],
    );
}

fn test_comb_display_conditional_write_excludes_lhs_even_on_unwritten_branch(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    sel: input logic,
    a: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = 8'd0;
        if sel {
            tmp = a;
        } else {
            $display("tmp=%0d", tmp);
        }
        out = tmp;
    }
}
"#, "Top");

    let sel = sim.signal("sel");
    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| {
        io.set(sel, 1u8);
        io.set(a, 9u8);
    })
    .unwrap();
    assert_eq!(sim.get_as::<u8>(out), 9);
    assert_eq!(sim.drain_runtime_events(), vec![]);

    sim.modify(|io| io.set(sel, 0u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "tmp=0".to_string(),
        }],
    );
}

fn test_comb_display_dynamic_port_alias_write_excludes_only_prefix(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Child (
    a: input logic,
    y: output logic,
) {
    always_comb {
        y = a;
    }
}

module Top (
    idx: input logic,
    a: input logic,
    b: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    always_comb {
        mem[2] = b;
    }

    inst child: Child (
        a: a,
        y: mem[1][idx],
    );

    always_comb {
        out = mem[2];
        $display("v=%0d", mem[2]);
    }
}
"#, "Top");

    let b = sim.signal("b");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(b, 0x44u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 0x44);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "v=68".to_string(),
        }],
    );
}

fn test_comb_display_duplicate_store_alias_keeps_capture_activation(sim) {
    @omit_veryl;
    @ignore_on(wasm);
    @build Simulator::builder(r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var x: logic<8>;
    var y: logic<8>;

    always_comb {
        x = a;
        y = a;
    }

    always_comb {
        out = y;
        $display("y=%0d", y);
    }
}
"#, "Top");

    let a = sim.signal("a");
    let out = sim.signal("out");

    sim.drain_runtime_events();

    sim.modify(|io| io.set(a, 7u8)).unwrap();
    assert_eq!(sim.get_as::<u8>(out), 7);
    assert_eq!(
        sim.drain_runtime_events(),
        vec![celox::RuntimeEvent::Display {
            message: "y=7".to_string(),
        }],
    );

    sim.modify(|io| io.set(a, 7u8)).unwrap();
    assert_eq!(sim.drain_runtime_events(), vec![]);
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
fn test_comb_observer_sensitivity_keeps_unwritten_dynamic_prefix_expansion() {
    let sim = Simulator::builder(
        r#"
module Top (
    a: input logic,
    idx: input logic,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        $display("v=%0d", tmp[idx]);
        tmp[0] = a;
        out = tmp;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let tmp_addr = sim.program().get_addr(&[], &["tmp"]).unwrap();
    let idx_addr = sim.program().get_addr(&[], &["idx"]).unwrap();
    let observer = &sim.program().comb_observers[0];

    // IEEE 1800-2023 9.2.2.2.1 uses expansions of the longest static prefix.
    // For tmp[idx], the dynamic select falls back to tmp's expansion, but the
    // written expression tmp[0] excludes only that written term. The unwritten
    // part of tmp must remain in the process sensitivity.
    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == idx_addr && atom.access.lsb == 0 && atom.access.msb == 0),
        "dynamic index expression should remain sensitive: {:?}",
        observer.sensitivity,
    );
    assert!(
        observer
            .sensitivity
            .iter()
            .all(|atom| atom.id != tmp_addr || atom.access.lsb > 0),
        "written tmp[0] must be excluded from sensitivity: {:?}",
        observer.sensitivity,
    );
    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == tmp_addr && atom.access.lsb <= 1 && atom.access.msb >= 1),
        "unwritten expansion terms of tmp must remain sensitive: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_dynamic_select_keeps_static_array_prefix() {
    let sim = Simulator::builder(
        r#"
module Top (
    idx: input logic,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    always_comb {
        $display("v=%0d", mem[1][idx]);
        out = 8'd0;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let observer = &sim.program().comb_observers[0];

    // IEEE 1800-2023 9.2.2.2.1 refers to the expansion of the longest static
    // prefix. For mem[1][idx], the longest static prefix is mem[1], not mem.
    assert!(
        observer
            .sensitivity
            .iter()
            .all(|atom| atom.id != mem_addr || (8..=15).contains(&atom.access.lsb)),
        "mem[1][idx] should not make other mem elements sensitive: {:?}",
        observer.sensitivity,
    );
    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == mem_addr && atom.access.lsb <= 8 && atom.access.msb >= 15),
        "mem[1][idx] should keep mem[1] sensitive: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_dynamic_write_excludes_only_static_prefix() {
    let sim = Simulator::builder(
        r#"
module Top (
    idx: input logic,
    a: input logic,
    b: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    always_comb {
        mem[2] = b;
    }

    always_comb {
        $display("v=%0d", mem[2]);
        mem[1][idx] = a;
        out = 8'd0;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let observer = &sim.program().comb_observers[0];

    // The written expression is mem[1][idx], whose longest static prefix is
    // mem[1]. Excluding written expressions must not remove mem[2].
    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == mem_addr && atom.access.lsb <= 16 && atom.access.msb >= 23),
        "mem[2] should remain sensitive despite dynamic write to mem[1]: {:?}",
        observer.sensitivity,
    );
    assert!(
        observer
            .sensitivity
            .iter()
            .all(|atom| atom.id != mem_addr || atom.access.msb < 8 || atom.access.lsb > 15),
        "written mem[1] should be excluded from sensitivity: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_indexed_part_select_keeps_static_prefix() {
    let sim = Simulator::builder(
        r#"
module Top (
    idx: input logic<3>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    always_comb {
        $display("v=%0d", mem[1][idx +: 2]);
        out = 8'd0;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let observer = &sim.program().comb_observers[0];

    // The indexed part-select anchor is dynamic, so the longest static prefix
    // is mem[1], not the bits selected when idx is treated as zero.
    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == mem_addr && atom.access.lsb <= 8 && atom.access.msb >= 15),
        "dynamic indexed part-select should keep all of mem[1] sensitive: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_function_output_dynamic_actual_excludes_only_prefix() {
    let sim = Simulator::builder(
        r#"
module Top (
    idx: input logic,
    a: input logic,
    b: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    function poke (
        value: input logic,
        dst: output logic,
    ) {
        dst = value;
    }

    always_comb {
        mem[2] = b;
    }

    always_comb {
        $display("v=%0d", mem[2]);
        poke(a, mem[1][idx]);
        out = 8'd0;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let observer = &sim.program().comb_observers[0];

    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == mem_addr && atom.access.lsb <= 16 && atom.access.msb >= 23),
        "function output dynamic actual must not remove mem[2]: {:?}",
        observer.sensitivity,
    );
    assert!(
        observer
            .sensitivity
            .iter()
            .all(|atom| atom.id != mem_addr || atom.access.msb < 8 || atom.access.lsb > 15),
        "function output dynamic actual should exclude written mem[1]: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_conditional_write_excludes_lhs_in_all_branches() {
    let sim = Simulator::builder(
        r#"
module Top (
    sel: input logic,
    a: input logic<8>,
    out: output logic<8>,
) {
    var tmp: logic<8>;

    always_comb {
        tmp = 8'd0;
        if sel {
            tmp = a;
        } else {
            $display("tmp=%0d", tmp);
        }
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
        "a written expression is excluded from always_comb sensitivity even if read on another branch: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_dynamic_part_select_write_excludes_only_static_prefix() {
    let sim = Simulator::builder(
        r#"
module Top (
    idx: input logic<3>,
    a: input logic<2>,
    b: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    always_comb {
        mem[2] = b;
    }

    always_comb {
        $display("v=%0d", mem[2]);
        mem[1][idx +: 2] = a;
        out = 8'd0;
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let observer = &sim.program().comb_observers[0];

    assert!(
        observer
            .sensitivity
            .iter()
            .any(|atom| atom.id == mem_addr && atom.access.lsb <= 16 && atom.access.msb >= 23),
        "dynamic part-select write must not remove mem[2]: {:?}",
        observer.sensitivity,
    );
    assert!(
        observer
            .sensitivity
            .iter()
            .all(|atom| atom.id != mem_addr || atom.access.msb < 8 || atom.access.lsb > 15),
        "dynamic part-select write should exclude written mem[1]: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_sensitivity_generate_static_index_stays_per_element() {
    let sim = Simulator::builder(
        r#"
module Top (
    out: output logic<8>[4],
) {
    var mem: logic<8>[4];

    always_comb {
        mem[0] = 8'd10;
        mem[1] = 8'd11;
        mem[2] = 8'd12;
        mem[3] = 8'd13;
    }

    for j in 0..4 :g_obs {
        always_comb {
            $display("v=%0d", mem[j]);
            out[j] = mem[j];
        }
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let mut observed_ranges: Vec<_> = sim
        .program()
        .comb_observers
        .iter()
        .map(|observer| {
            observer
                .sensitivity
                .iter()
                .filter(|atom| atom.id == mem_addr)
                .map(|atom| (atom.access.lsb, atom.access.msb))
                .collect::<Vec<_>>()
        })
        .collect();
    observed_ranges.sort();

    assert_eq!(
        observed_ranges,
        vec![vec![(0, 7)], vec![(8, 15)], vec![(16, 23)], vec![(24, 31)],],
        "generated static indices should not fall back to whole mem sensitivity",
    );
}

#[test]
fn test_comb_observer_sensitivity_dynamic_port_alias_write_excludes_only_prefix() {
    let sim = Simulator::builder(
        r#"
module Child (
    a: input logic,
    y: output logic,
) {
    always_comb {
        y = a;
    }
}

module Top (
    idx: input logic,
    a: input logic,
    b: input logic<8>,
    out: output logic<8>,
) {
    var mem: logic<8>[4];

    always_comb {
        mem[2] = b;
    }

    inst child: Child (
        a: a,
        y: mem[1][idx],
    );

    always_comb {
        out = 8'd0;
        $display("v=%0d", mem[2]);
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let mem_addr = sim.program().get_addr(&[], &["mem"]).unwrap();
    let observer =
        sim.program()
            .comb_observers
            .iter()
            .find(|observer| {
                observer.sensitivity.iter().any(|atom| {
                    atom.id == mem_addr && atom.access.lsb <= 16 && atom.access.msb >= 23
                })
            })
            .expect("missing observer sensitive to mem[2]");

    assert!(
        observer
            .sensitivity
            .iter()
            .all(|atom| atom.id != mem_addr || atom.access.msb < 8 || atom.access.lsb > 15),
        "dynamic port alias write should exclude only written mem[1]: {:?}",
        observer.sensitivity,
    );
}

#[test]
fn test_comb_observer_duplicate_store_alias_preserves_capture_enabled_store() {
    let sim = Simulator::builder(
        r#"
module Top (
    a: input logic<8>,
    out: output logic<8>,
) {
    var x: logic<8>;
    var y: logic<8>;

    always_comb {
        x = a;
        y = a;
    }

    always_comb {
        out = y;
        $display("y=%0d", y);
    }
}
"#,
        "Top",
    )
    .build()
    .unwrap();

    let y_addr = sim.program().get_addr(&[], &["y"]).unwrap();
    assert!(
        !sim.program().address_aliases.contains_key(&y_addr),
        "capture-enabled duplicate store must not be removed as an alias: {:?}",
        sim.program().address_aliases,
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
    let capture = sir
        .find("CombCaptureEvent(")
        .unwrap_or_else(|| panic!("missing comb capture event in SIR:\n{sir}"));
    assert!(
        store_low < capture,
        "observer capture should be ordered after the prior low-bit write; unrelated later writes may move before capture if the statement-position value is snapshotted:\n{sir}"
    );
}

#[test]
fn test_comb_observer_store_activation_lowers_as_separate_instruction() {
    let result = Simulator::builder(
        r#"
module Top (
    a: input logic,
    out: output logic,
) {
    var tmp: logic;

    always_comb {
        tmp = a;
    }

    always_comb {
        out = tmp;
        $display("tmp=%0d", tmp);
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

    let old_load = sir
        .find("Load(addr=tmp (region=0), offset=0, bits=1)")
        .unwrap_or_else(|| panic!("missing old-value load for comb observer activation:\n{sir}"));
    let store = sir
        .find("Store(addr=tmp (region=0), offset=0, bits=1")
        .unwrap_or_else(|| panic!("missing tmp store in SIR:\n{sir}"));
    let enable = sir
        .find("CombCaptureEnableIfChanged(")
        .unwrap_or_else(|| panic!("missing separated comb observer enable instruction:\n{sir}"));
    let capture = sir
        .find("CombCaptureEvent(")
        .unwrap_or_else(|| panic!("missing comb capture event in SIR:\n{sir}"));

    assert!(
        old_load < store && store < enable && enable < capture,
        "observer activation should be a separate instruction ordered around the observed store:\n{sir}"
    );
    assert!(
        !sir.contains("comb_capture_sites=[0]"),
        "observer activation sites should not be attached to Store:\n{sir}"
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
    let first_capture = sir
        .find("CombCaptureEvent(site=0")
        .unwrap_or_else(|| panic!("missing first capture event in SIR:\n{sir}"));
    let store_bit1 = sir
        .find("Store(addr=tmp (region=0), offset=1, bits=1")
        .unwrap_or_else(|| panic!("missing bit 1 store in SIR:\n{sir}"));
    let second_capture = sir
        .find("CombCaptureEvent(site=1")
        .unwrap_or_else(|| panic!("missing second capture event in SIR:\n{sir}"));

    assert!(
        store_bit0 < first_capture && store_bit1 < second_capture && first_capture < second_capture,
        "each observer capture should preserve prior-write and side-effect order; unrelated later writes may move before capture if statement-position values are snapshotted:\n{sir}"
    );
}

#[test]
fn test_comb_observer_dynamic_for_lowers_capture_event() {
    let result = Simulator::builder(
        r#"
module Top (
    count: input logic<4>,
    base: input logic<8>,
    out: output logic<8>,
) {
    var sum: logic<8>;

    always_comb {
        sum = base;
        for i in 0..count {
            sum = sum + i;
            $display("i=%0d sum=%0d", i, sum);
        }
        out = sum;
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

    assert!(
        sir.contains("CombCaptureEvent("),
        "dynamic for observer should lower to a comb capture event:\n{sir}"
    );
}
