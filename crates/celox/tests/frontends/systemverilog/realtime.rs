use super::*;

sv_backends! {
    fn fractional_time_uses_the_declaring_scope(sim) {
        @setup {
            let source = r#"package P;
  timeunit 100ps / 1ps;
  function automatic real now(); return $realtime; endfunction
endpackage
module Child(input logic clk, output logic [63:0] sampled);
  timeunit 10ns / 1ps;
  always_ff @(posedge clk) sampled = $realtobits($realtime());
endmodule
module Top(input logic clk, input int a,
           output logic [63:0] sampled, child_time, package_time, live,
           output int rounded, truncated, width);
  timeunit 1ns / 1ps;
  Child child(.clk(clk), .sampled(child_time));
  always_ff @(posedge clk) begin
    sampled = $realtobits($realtime);
    package_time = $realtobits(P::now());
    rounded = $realtime;
    truncated = $rtoi($realtime);
    width = $bits($realtime);
  end
  always_comb live = $realtobits($realtime + a);
endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("realtime_scope.sv"))], "Top");
        sim.run_until(0).unwrap();
        let clk = sim.signal("clk");
        let a = sim.signal("a");
        sim.run_until(1250).unwrap();
        sim.modify(|io| { io.set(clk, 1u8); io.set(a, 1u8); }).unwrap();
        sim.run_until(1250).unwrap();
        for (name, bits) in [("sampled", 1.25f64.to_bits()), ("child_time", 0.125f64.to_bits()),
                             ("package_time", 12.5f64.to_bits()), ("live", 2.25f64.to_bits())] {
            assert_eq!(sim.get(sim.signal(name)), BigUint::from(bits), "{name}");
        }
        assert_eq!(sim.get(sim.signal("rounded")), BigUint::from(1u8));
        assert_eq!(sim.get(sim.signal("truncated")), BigUint::from(1u8));
        assert_eq!(sim.get(sim.signal("width")), BigUint::from(64u8));
        sim.modify(|io| io.set(clk, 0u8)).unwrap();
        sim.run_until(1500).unwrap();
        assert_eq!(sim.get(sim.signal("live")), BigUint::from(2.25f64.to_bits()));
        sim.modify(|io| { io.set(clk, 1u8); io.set(a, 0u8); }).unwrap();
        sim.run_until(1500).unwrap();
        assert_eq!(sim.get(sim.signal("sampled")), BigUint::from(1.5f64.to_bits()));
        assert_eq!(sim.get(sim.signal("live")), BigUint::from(1.5f64.to_bits()));
        assert_eq!(sim.get(sim.signal("rounded")), BigUint::from(2u8));
        assert_eq!(sim.get(sim.signal("truncated")), BigUint::from(1u8));
    }
    fn realtime_returns_real_in_timed_processes(sim) {
        @case "realtime::realtime_returns_real_in_timed_processes";
    }
    fn realtime_keeps_the_full_tick_counter(sim) {
        @case "realtime::realtime_keeps_the_full_tick_counter";
    }
    fn realtime_checkpoint_restores_fractional_time(sim) {
        @setup {
            let source = r#"module Top(output logic [63:0] sampled);
                timeunit 1ns / 1ps;
                initial begin
                    #1250ps sampled = $realtobits($realtime);
                    #250ps sampled = $realtobits($realtime);
                end
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("realtime_checkpoint.sv"))], "Top");
        sim.run_until(1250).unwrap();
        let sampled = sim.signal("sampled");
        assert_eq!(sim.get(sampled), BigUint::from(1.25f64.to_bits()));
        let checkpoint = sim.checkpoint().unwrap();
        sim.run_until(1500).unwrap();
        assert_eq!(sim.get(sampled), BigUint::from(1.5f64.to_bits()));
        sim.restore(&checkpoint).unwrap();
        assert_eq!(sim.get(sampled), BigUint::from(1.25f64.to_bits()));
        sim.run_until(1500).unwrap();
        assert_eq!(sim.get(sampled), BigUint::from(1.5f64.to_bits()));
    }
    fn realtime_converts_unsigned_64_bit_ticks(sim) {
        @setup {
            let source = r#"module Top(input bit clk, output logic [63:0] sampled);
                timeunit 1ps / 1ps;
                always_ff @(posedge clk) sampled = $realtobits($realtime);
            endmodule"#;
        }
        @build celox::Simulation::from_sv_sources(vec![(source, Path::new("realtime_unsigned.sv"))], "Top");
        sim.run_until(0).unwrap();
        let time = (1u64 << 63) + 2048;
        sim.run_until(time).unwrap();
        let clk = sim.signal("clk");
        sim.modify(|io| io.set(clk, 1u8)).unwrap();
        sim.run_until(time).unwrap();
        assert_eq!(sim.get(sim.signal("sampled")), BigUint::from((time as f64).to_bits()));
    }
}

#[test]
fn realtime_is_nonconstant_and_checks_arguments() {
    for source in [
        "module Top; localparam real T = $realtime; endmodule",
        "module Top; real t; initial t = $realtime(1); endmodule",
        "module Top; real t; initial t = $realtime(,); endmodule",
    ] {
        let error =
            match Simulator::from_sv_sources(vec![(source, Path::new("bad_realtime.sv"))], "Top")
                .build_interpreter()
            {
                Ok(_) => panic!("accepted {source}"),
                Err(error) => error.to_string(),
            };
        assert!(error.contains("$realtime"), "{error}");
    }
}
