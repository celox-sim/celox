#[test]
fn mixed_active_and_nba_clock_inputs_preserve_both_edges() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0, enable = 1, active_value = 0;
    bit [7:0] edges = 0;
    wire first = clk & enable;
    wire second = first ^ active_value;
    always #1 clk = !clk;
    always @(posedge clk) enable <= 0;
    always @(posedge first) active_value = 1;
    always @(posedge second) edges = edges + 1;
    initial begin #2; if (edges != 2) $fatal(1, "lost repeated edge"); $finish; end
endmodule"#;
    for four_state in [false, true] {
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("repeated-active.sv"))],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        simulation.run_until(3).unwrap();
        assert!(simulation.is_finished());
        let edges = simulation.signal("edges");
        assert_eq!(simulation.get(edges), num_bigint::BigUint::from(2u8));
        #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
        assert!(
            simulation.scheduler_stats().compiled_slots > 0,
            "{:?}",
            simulation.scheduler_stats()
        );
    }
}

/// With a partitioned settle, a process's edge wait is checked only when
/// a signal it watches changed, whoever changed it: a clock toggled by a
/// delay process, a register, an asynchronous reset, combinational logic.
/// Every waiter wakes as in the reference scheduler.
#[test]
fn partitioned_settle_wait_flags_wake_every_waiter() {
    let source = r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    bit rst_n = 0;
    bit [7:0] count = 0;
    bit [7:0] twice_seen = 0;
    bit [7:0] odd_seen = 0;
    bit [7:0] resets = 0;
    wire [7:0] twice = count + count;
    wire odd = count[0];
    always #1 clk = !clk;
    initial begin #3; rst_n = 1; #20; rst_n = 0; #4; rst_n = 1; end
    always @(posedge clk or negedge rst_n) if (!rst_n) count = 0; else count = count + 1;
    always @(twice) twice_seen = twice_seen + 1;
    always @(posedge odd) odd_seen = odd_seen + 1;
    always @(negedge rst_n) resets = resets + 1;
    initial begin #40; $finish; end
endmodule"#;
    let run = |partitioned: bool| {
        let builder = crate::Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("wait-flags.sv"))],
            "Top",
        )
        .scheduler_mode(crate::SchedulerMode::Differential);
        let builder = if partitioned {
            builder
                .settle_partition(crate::SettlePartition::Always)
                .settle_check(true)
        } else {
            builder.disable_pass(crate::SirPass::PartitionedSettle)
        };
        let mut simulation = builder.build_native().unwrap();
        simulation.run_until(41).unwrap();
        assert!(simulation.is_finished());
        let stats = simulation.scheduler_stats();
        assert_eq!(stats.unchecked, None, "{stats:?}");
        assert!(stats.checked_steps > 0, "{stats:?}");
        #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
        assert!(stats.compiled_slots > 0, "{stats:?}");
        ["count", "twice_seen", "odd_seen", "resets"].map(|name| {
            let signal = simulation.signal(name);
            simulation.get(signal)
        })
    };
    let partitioned = run(true);
    assert_eq!(partitioned, run(false));
    assert!(
        partitioned
            .iter()
            .all(|value| *value != num_bigint::BigUint::ZERO),
        "{partitioned:?}"
    );
}

#[test]
fn unknown_then_known_clock_counts_both_four_state_posedges() {
    for four_state in [false, true] {
        let expected = if four_state { 2 } else { 1 };
        let source = format!(
            r#"`timescale 1ns/1ns
module Top;
    bit clk = 0;
    logic enable = 1'bx;
    bit [7:0] edges = 0;
    wire gated = clk & enable;
    always #1 clk = !clk;
    always @(posedge clk) enable = 1;
    always @(posedge gated) edges = edges + 1;
    initial begin #2; if (edges != {expected}) $fatal(1, "bad unknown edges"); $finish; end
endmodule"#
        );
        let mut simulation = crate::Simulation::from_sv_sources(
            vec![(source.as_str(), std::path::Path::new("unknown-active.sv"))],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
        .build_native()
        .unwrap();
        simulation.run_until(3).unwrap();
        assert!(simulation.is_finished());
        let edges = simulation.signal("edges");
        assert_eq!(
            simulation.get(edges),
            num_bigint::BigUint::from(expected as u8)
        );
        #[cfg(all(target_arch = "x86_64", not(feature = "arm64-codegen")))]
        if four_state {
            assert!(
                simulation.scheduler_stats().compiled_slots > 0,
                "{:?}",
                simulation.scheduler_stats()
            );
        }
    }
}
