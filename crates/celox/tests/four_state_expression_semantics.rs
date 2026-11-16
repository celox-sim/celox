use celox::{BigUint, Simulator};

#[path = "test_utils/mod.rs"]
#[macro_use]
mod test_utils;

all_backends! {

    fn ff_assert_uses_procedural_four_state_truth(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    clk: input clock,
    cond: input logic<130>,
) {
    always_ff (clk) {
        $assert_continue(cond, "bad condition");
    }
}
"#, "Top").four_state(true);

        let clk = sim.event("clk");
        let cond = sim.signal("cond");
        let known_one = BigUint::from(1u8) << 100usize;
        let unknown = BigUint::from(1u8) << 2usize;

        sim.modify(|io| {
            io.set_four_state(cond, &known_one | &unknown, unknown.clone());
        })
        .unwrap();
        sim.tick(clk).unwrap();
        assert!(bare(sim.drain_runtime_events()).is_empty());

        for (value, mask, label) in [
            (BigUint::from(0u8), BigUint::from(0u8), "0"),
            (unknown.clone(), unknown.clone(), "X"),
            (BigUint::from(0u8), unknown.clone(), "Z"),
        ] {
            sim.modify(|io| {
                io.set_four_state(cond, value.clone(), mask.clone());
            })
            .unwrap();
            sim.tick(clk).unwrap();
            assert_eq!(
                bare(sim.drain_runtime_events()),
                vec![celox::RuntimeEvent::Report(celox::Report::new(celox::Severity::Error, b"bad condition".to_vec()))],
                "cond={label}",
            );
        }
    }

    fn comb_assert_uses_procedural_four_state_truth(sim) {
        @omit_veryl;
        @build Simulator::builder(r#"
module Top (
    cond: input logic<130>,
) {
    always_comb {
        $assert_continue(cond, "bad condition");
    }
}
"#, "Top").four_state(true);

        let cond = sim.signal("cond");
        let known_one = BigUint::from(1u8) << 100usize;
        let unknown = BigUint::from(1u8) << 2usize;
        bare(sim.drain_runtime_events());

        for (unknown_value, label) in [(1u8, "X"), (0u8, "Z")] {
            sim.modify(|io| {
                io.set_four_state(
                    cond,
                    &known_one | (&unknown * unknown_value),
                    unknown.clone(),
                );
            })
            .unwrap();
            assert!(
                bare(sim.drain_runtime_events()).is_empty(),
                "known one plus {label} must pass",
            );

            sim.modify(|io| {
                io.set_four_state(cond, &unknown * unknown_value, unknown.clone());
            })
            .unwrap();
            assert_eq!(
                bare(sim.drain_runtime_events()),
                vec![celox::RuntimeEvent::Report(celox::Report::new(celox::Severity::Error, b"bad condition".to_vec()))],
                "only {label} must fail",
            );
        }
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
