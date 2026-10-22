use celox::{
    AddrLookupError, BigUint, Report, RuntimeErrorCode, RuntimeEvent, Severity, Simulator,
};

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
        assert_eq!(bare(sim.drain_runtime_events()), vec![RuntimeEvent::Report(Report::new(Severity::Fatal, b"site zero".to_vec()))]);
        assert!(bare(sim.drain_runtime_events()).is_empty());
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
        assert_eq!(bare(sim.drain_runtime_events()), vec![
            RuntimeEvent::Display { message: b"first".to_vec() },
            RuntimeEvent::Display { message: b"second".to_vec() },
            RuntimeEvent::Report(Report::new(Severity::Fatal, b"site two".to_vec())),
        ]);
        assert!(bare(sim.drain_runtime_events()).is_empty());
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
        bare(sim.drain_runtime_events());
        let mut drain = sim.runtime_event_drain().expect("drain handle");
        let fatal = vec![RuntimeEvent::Report(Report::new(Severity::Fatal, b"input fatal".to_vec()))];

        sim.set(a, 1u8);
        assert_eq!(bare(drain.drain()), fatal);
        assert!(bare(drain.drain()).is_empty());
        sim.set(a, 0u8);
        sim.set_wide(w, BigUint::from(1u8));
        assert_eq!(bare(drain.drain()), fatal);
        sim.set_wide(w, BigUint::from(0u8));
        sim.set_four_state(a, BigUint::from(1u8), BigUint::from(0u8));
        assert_eq!(bare(drain.drain()), fatal);
        sim.set(a, 0u8);
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(bare(drain.drain()), fatal);
        assert!(bare(drain.drain()).is_empty());
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
        bare(sim.drain_runtime_events());
        // Save the failing input before lazy evaluation captures its assertion.
        sim.set(a, 1u8);
        let checkpoint = sim.checkpoint().unwrap();
        sim.set(a, 0u8);
        bare(sim.drain_runtime_events());
        let mut drain = sim.runtime_event_drain().expect("drain handle");
        sim.restore(&checkpoint).unwrap();
        assert_eq!(bare(drain.drain()), vec![RuntimeEvent::Report(Report::new(Severity::Fatal, b"restored fatal".to_vec()))]);
        assert!(bare(drain.drain()).is_empty());
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

        assert_eq!(bare(sim.drain_runtime_events()), vec![
            celox::RuntimeEvent::Display { message: b"before".to_vec() },
            celox::RuntimeEvent::Report(celox::Report::new(celox::Severity::Fatal, b"initial fatal".to_vec())),
        ]);
        assert!(bare(sim.drain_runtime_events()).is_empty());
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

        bare(sim.drain_runtime_events());
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 1u8)).unwrap();
        assert_eq!(bare(sim.drain_runtime_events_with_context(celox::RuntimeFormatContext {
            scope: Some("host.Top"),
            ..Default::default()
        })), vec![celox::RuntimeEvent::Report(celox::Report::new(celox::Severity::Fatal, b"fatal host.Top".to_vec()))]);
        assert!(bare(sim.drain_runtime_events()).is_empty());
        sim.modify(|io| io.set(a, 2u8)).unwrap();
        assert_eq!(bare(sim.drain_runtime_events()), vec![
            celox::RuntimeEvent::Display { message: b"after".to_vec() },
        ]);
    }
}

/// The drained events without the time, scope, location and assertion of
/// their reports, which these tests do not check.
fn bare(events: Vec<celox::RuntimeEvent>) -> Vec<celox::RuntimeEvent> {
    events
        .into_iter()
        .map(|event| match event {
            celox::RuntimeEvent::Report(report) => {
                celox::RuntimeEvent::Report(celox::Report::new(report.severity, report.message))
            }
            event => event,
        })
        .collect()
}
