fn check<B: crate::SimBackend>(backend: &str, mut sim: crate::Simulation<B>) {
    sim.run_until(100)
        .unwrap_or_else(|error| panic!("{backend}: {error}"));
    assert!(sim.is_finished(), "{backend}");
    assert_eq!(sim.time(), 14, "{backend}");
    let stats = sim.scheduler_stats();
    assert!(stats.compiled_slots > 0, "{backend}: {stats:?}");
    assert!(stats.checked_steps > 0, "{backend}: {stats:?}");
    assert!(stats.unchecked.is_none(), "{backend}: {stats:?}");
}

// Compare the final memory image and runtime events, including blocking
// writes in later FF partitions and NBAs that must remain unpublished.
#[test]
fn finish_observes_the_complete_ff_evaluation_phase() {
    for four_state in [false, true] {
        for source in [
            r#"`timescale 1ns/1ns
module Top;
 bit clk=0; int count=0;
 always #2 clk=~clk;
 always @(posedge clk) count<=count+1;
 always @(posedge clk) if(count==3) $finish;
 final $display("FINAL count=%0d",count);
endmodule
"#,
            r#"`timescale 1ns/1ns
module Top;
 bit clk=0;
 int count=0;
 int cycles=0;
 always #2 clk=~clk;
 always @(posedge clk) count <= count + 1;
 always @(posedge clk) cycles++;
 always @(posedge clk) if (count==3) begin
   $display("FINISH count=%0d cycles=%0d",count,cycles);
   $finish;
 end
 final $display("FINAL count=%0d cycles=%0d",count,cycles);
endmodule
"#,
            r#"`timescale 1ns/1ns
module Utils;
 longint unsigned cycles=0;
 always @(posedge $root.Top.clk) ++cycles;
 final $display("FINAL cycles=%0d",cycles);
endmodule
module Top;
 Utils u();
 bit clk=0;
 bit en=0; initial begin #1 en=1; #30 en=0; end
 wire hfclk=clk & en;
 int count=0; int active=0; int seen=0;
 always @(posedge hfclk) active = active+1;
 initial forever @(active) seen++;
 always #2 clk=~clk;
 always @(posedge hfclk) count <= count + 1;
 always @(posedge hfclk) if (count==3) begin
   $display("FINISH count=%0d",count);
   $finish;
 end
endmodule
"#,
        ] {
            let builder = || {
                crate::Simulation::from_sv_sources(
                    vec![(source, std::path::Path::new("finish-phase.sv"))],
                    "Top",
                )
                .four_state(four_state)
                .scheduler_mode(crate::SchedulerMode::DifferentialRuns)
            };
            check("native", builder().build().unwrap());
            #[cfg(feature = "cranelift")]
            check("cranelift", builder().build_cranelift().unwrap());
            #[cfg(feature = "wasm-runtime")]
            check("wasm", builder().build_wasm().unwrap());
        }
    }
}
