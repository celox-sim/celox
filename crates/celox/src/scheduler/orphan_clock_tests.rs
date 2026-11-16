use crate::{KernelSchedule, SchedulerMode, SimBackend, Simulation};

fn check<B: SimBackend>(mut sim: Simulation<B>, stops: &[u64], expected: &[(&str, u8)]) {
    for &stop in stops {
        sim.run_until(stop).unwrap();
    }
    assert!(sim.is_finished());
    let stats = sim.scheduler_stats();
    assert!(stats.compiled_slots > 0, "{stats:?}");
    assert!(stats.checked_steps > 0, "{stats:?}");
    assert!(stats.unchecked.is_none(), "{stats:?}");
    assert!(
        stats.kernels.iter().all(|k| *k == KernelSchedule::Compiled),
        "{stats:?}"
    );
    for &(name, value) in expected {
        let signal = sim.signal(name);
        assert_eq!(sim.get(signal), value.into(), "{name}");
    }
}

fn check_backends(source: &str, four_state: bool, stops: &[u64], expected: &[(&str, u8)]) {
    let build = || {
        Simulation::from_sv_sources(
            vec![(source, std::path::Path::new("orphan-clock.sv"))],
            "Top",
        )
        .four_state(four_state)
        .scheduler_mode(SchedulerMode::DifferentialRuns)
    };
    check(build().build_native().unwrap(), stops, expected);
    #[cfg(feature = "cranelift")]
    check(build().build_cranelift().unwrap(), stops, expected);
    #[cfg(feature = "wasm-runtime")]
    check(build().build_wasm().unwrap(), stops, expected);
}

// An unrelated source-clock FF must not decide whether the entire design
// can run compiled. Include a concurrent independent clock and a divided
// domain downstream of the clock whose source has no event of its own.
#[test]
fn combinational_clocks_do_not_require_source_domains() {
    for definition in [
        "assign gclk = clk & en;",
        "assign gclk = en ? clk : 1'b0;",
        "assign gclk = ~clk;",
        "always_comb gclk = clk & en;",
        "logic held=0; always_latch if (!clk) held=en; assign gclk=clk & held;",
        "wire gate=clk & en; logic half=0; always_ff @(posedge gate) half<=~half; assign gclk=half;",
    ] {
        for source_domain in ["", "always_ff @(posedge clk) dummy<=~dummy;"] {
            let source = format!(
                r#"`timescale 1ns/1ns
module Top;
 logic clk=0; logic en=0; logic gclk;
 logic other_clk=0; logic [7:0] other=0; logic dummy=0;
 logic [7:0] q=0;
 always #5 clk=~clk;
 always #7 other_clk=~other_clk;
 initial forever #17 en=~en;
 always_ff @(posedge other_clk) other<=other+1;
 {source_domain}
 {definition}
 always_ff @(posedge gclk) q<=q+1;
 initial begin #400 $display("Q %d other %d dummy %d",q,other,dummy); $finish; end
endmodule
"#
            );
            for four_state in [false, true] {
                check_backends(&source, four_state, &[100, 333, 500], &[]);
            }
        }
    }
}

#[test]
fn orphan_clock_edges_preserve_unknown_transitions() {
    let source = r#"`timescale 1ns/1ns
module Top;
 logic clk=0; logic en=1; wire gclk=clk & en;
 logic [7:0] p=0, n=0;
 always_ff @(posedge gclk) p<=p+1;
 always_ff @(negedge gclk) n<=n+1;
 initial begin #5 clk=1'bx; #5 clk=1; #5 clk=1'bz; #5 clk=0; #5 clk=1; #5 clk=0; end
 initial begin #35 en=0; #5 $display("p %d n %d",p,n); $finish; end
endmodule
"#;
    // IEEE 1800-2023 Table 9-2: three rises and three falls in the
    // sequence, plus the initial settle's X-to-0 falling edge.
    check_backends(source, true, &[10, 25, 50], &[("p", 3), ("n", 4)]);
}
