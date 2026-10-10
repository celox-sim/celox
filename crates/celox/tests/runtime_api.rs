use celox::{AddrLookupError, BigUint, RuntimeErrorCode, RuntimeEvent, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
    fn initial_fatal_at_site_zero_is_an_error_and_can_be_drained(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top { always_comb { $assert(1'd0, "site zero"); } }
"#, "Top");
        assert!(matches!(sim.eval_comb(), Err(RuntimeErrorCode::Runtime { message, .. }) if message == "site zero"));
        assert_eq!(sim.drain_runtime_events(), vec![RuntimeEvent::AssertFatal { message: "site zero".to_string() }]);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn drain_returns_fatal_after_two_display_sites(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top {
    always_comb {
        $display("first");
        $display("second");
        $assert(1'd0, "site two");
        $display("unreachable");
    }
}
"#, "Top");
        assert!(matches!(sim.eval_comb(), Err(RuntimeErrorCode::Runtime { message, .. }) if message == "site two"));
        assert_eq!(sim.drain_runtime_events(), vec![
            RuntimeEvent::Display { message: "first".to_string() },
            RuntimeEvent::Display { message: "second".to_string() },
            RuntimeEvent::AssertFatal { message: "site two".to_string() },
        ]);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn drain_handle_keeps_fatal_records_from_all_input_mutations(sim) {
        @omit_veryl;
        // WASM owns its linear-memory buffer and cannot lend a drain handle.
        @ignore_on(wasm);
        @build Simulator::builder(r#"
module Top (a: input logic<8>, w: input logic<128>) {
    always_comb { $assert(a != 8'd1 && w != 128'd1, "input fatal"); }
}
"#, "Top");
        let a = sim.signal("a");
        let w = sim.signal("w");
        sim.modify(|io| { io.set(a, 0u8); io.set(w, 0u8); }).unwrap();
        sim.drain_runtime_events();
        let mut drain = sim.runtime_event_drain().expect("drain handle");
        let fatal = vec![RuntimeEvent::AssertFatal { message: "input fatal".to_string() }];

        sim.set(a, 1u8);
        assert_eq!(drain.drain(), fatal);
        assert!(drain.drain().is_empty());
        sim.set(a, 0u8);
        sim.set_wide(w, BigUint::from(1u8));
        assert_eq!(drain.drain(), fatal);
        sim.set_wide(w, BigUint::from(0u8));
        sim.set_four_state(a, BigUint::from(1u8), BigUint::from(0u8));
        assert_eq!(drain.drain(), fatal);
        sim.set(a, 0u8);
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(drain.drain(), fatal);
        assert!(drain.drain().is_empty());
    }

    fn drain_handle_keeps_fatal_record_after_checkpoint_restore(sim) {
        @omit_veryl;
        @ignore_on(wasm);
        @build Simulator::builder(r#"
module Top (a: input logic<8>) {
    always_comb { $assert(a != 8'd1, "restored fatal"); }
}
"#, "Top");
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 0u8)).unwrap();
        sim.drain_runtime_events();
        // Save the failing input before lazy evaluation captures its assertion.
        sim.set(a, 1u8);
        let checkpoint = sim.checkpoint().unwrap();
        sim.set(a, 0u8);
        sim.drain_runtime_events();
        let mut drain = sim.runtime_event_drain().expect("drain handle");
        sim.restore(&checkpoint).unwrap();
        assert_eq!(drain.drain(), vec![RuntimeEvent::AssertFatal { message: "restored fatal".to_string() }]);
        assert!(drain.drain().is_empty());
    }

    fn try_event_rejects_signals_without_events(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    rst: input reset,
    d: input logic<8>,
    q: output logic<8>,
) {
    always_ff (clk, rst) {
        if_reset { q = 0; } else { q = d; }
    }
}
"#, "Top");

        assert!(sim.try_event("clk").is_ok());
        assert!(sim.try_event("rst").is_ok());
        for port in ["d", "q"] {
            let result = sim.try_event(port);
            assert!(
                matches!(&result, Err(AddrLookupError::NotAnEvent { path }) if path == port),
                "{port}: {result:?}",
            );
        }
        assert!(matches!(
            sim.try_event("missing"),
            Err(AddrLookupError::VariableNotFound { .. }),
        ));
    }

    fn drain_runtime_events_returns_initial_comb_fatal(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top {
    always_comb {
        $display("before");
        $assert(1'd0, "initial fatal");
        $display("after");
    }
}
"#, "Top");

        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: "before".to_string() },
            celox::RuntimeEvent::AssertFatal { message: "initial fatal".to_string() },
        ]);
        assert!(sim.drain_runtime_events().is_empty());
    }

    fn drain_runtime_events_with_context_returns_comb_fatal_after_modify(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (a: input logic<8>, y: output logic<8>) {
    always_comb {
        y = a;
        $assert(a != 8'd1, "fatal %m");
        $display("after");
    }
}
"#, "Top");

        sim.drain_runtime_events();
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(sim.drain_runtime_events_with_context(celox::RuntimeFormatContext {
            scope: Some("host.Top"),
            ..Default::default()
        }), vec![celox::RuntimeEvent::AssertFatal { message: "fatal host.Top".to_string() }]);
        assert!(sim.drain_runtime_events().is_empty());
        sim.modify(|io| io.set(a, 2u8)).unwrap();
        assert_eq!(sim.drain_runtime_events(), vec![
            celox::RuntimeEvent::Display { message: "after".to_string() },
        ]);
    }
}
