#[test]
fn unobserved_blocking_results_keep_clock_and_waiter_compiled() {
    let source = r#"`timescale 1ns/1ns
module Top;
            bit clk = 0;
            bit [7:0] count = 0, q = 0;
            always #1 clk = !clk;
            always @(posedge clk) begin count = count + 1; q <= count; end
            initial begin
                repeat (10) @(negedge clk);
                if (count != 10 || q != 10) $fatal(1, "bad count");
                $finish;
            end
        endmodule"#;
    for four_state in [false, true] {
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("active-counter.sv"))],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        simulation.run_until(24).unwrap();
        assert!(simulation.is_finished());
        let stats = simulation.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert!(stats.checked_steps > 0, "{stats:?}");
    }
}

#[test]
fn blocking_gate_update_keeps_its_edge_before_nba() {
    let source = r#"`timescale 1ns/1ns
module Top;
            bit clk = 0, enable = 0, a = 0, seen = 0;
            wire gated = clk & enable;
            always #1 clk = !clk;
            always @(posedge clk) begin enable = 1; a <= 1; end
            always @(posedge gated) seen <= a;
            initial begin #2; if (seen != 0) $fatal(1, "NBA published before active edge"); $finish; end
        endmodule"#;
    for four_state in [false, true] {
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("active-gate.sv"))],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        simulation.run_until(4).unwrap();
        assert!(simulation.is_finished());
        let seen = simulation.signal("seen");
        assert_eq!(simulation.get(seen), num_bigint::BigUint::from(0u8));
        let stats = simulation.scheduler_stats();
        assert!(stats.compiled_slots > 0, "{stats:?}");
        assert!(stats.checked_steps > 0, "{stats:?}");
    }
}
