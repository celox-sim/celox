use celox::{AddrLookupError, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {
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
