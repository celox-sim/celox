use super::*;

sv_backends! {
    fn combinational_real_display_and_assertion_messages(sim) {
        @setup { let source = r#"
            module Top(input logic enabled, input logic [63:0] bits);
                real x;
                assign x = $bitstoreal(bits);
                always_comb if (enabled) begin
                    $display("comb=%.2f expression=%.1f", x, x * 2.0);
                    assert (x < 0.0) else $error("assert=%.2f", x);
                end
            endmodule
        "#; }
        @build Simulator::from_sv_sources(vec![(source, Path::new("real_comb_display.sv"))], "Top").four_state(true);
        bare(sim.drain_runtime_events());
        let enabled = sim.signal("enabled");
        let bits = sim.signal("bits");
        sim.modify(|io| { io.set(bits, 0x3ff8000000000000u64); io.set(enabled, 1u8); }).unwrap();
        assert_eq!(sim.get(sim.signal("x")), BigUint::from(0x3ff8000000000000u64));
        let events = bare(sim.drain_runtime_events());
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events.contains(&celox::RuntimeEvent::Display { message: b"comb=1.50 expression=3.0".to_vec() }), "{events:?}");
        assert!(events.contains(&celox::RuntimeEvent::Report(celox::Report::new(celox::Severity::Error, b"assert=1.50".to_vec()))), "{events:?}");
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
