#![cfg(feature = "systemverilog")]
use std::path::Path;

const DISTRIBUTIONS: [(&str, &str, i32, i32); 6] = [
    ("$dist_normal", "0, 10", -4, -642_763_923),
    ("$dist_exponential", "10", 82, 1_174_174),
    ("$dist_poisson", "10", 3, -642_763_923),
    ("$dist_chi_square", "5", 2, -1_369_077_693),
    ("$dist_t", "5", 0, -434_453_879),
    ("$dist_erlang", "3, 10", 33, 861_173_020),
];

fn actuals_source() -> String {
    let mut body = String::new();
    for (name, parameters, expected, next) in DISTRIBUTIONS {
        body.push_str(&format!(
            r#"
          seeds[0] = 17; seeds[1] = 91; calls = 0;
          value = {name}(seeds[index()], {parameters});
          ok &= value == {expected} && calls == 1 && seeds[0] == {next} && seeds[1] == 91;
          narrow = -1; reference = -1;
          value = {name}(narrow, {parameters}); other = {name}(reference, {parameters});
          ok &= value == other && narrow == reference[7:0];
          unsigned_seed = 255; reference = 255;
          value = {name}(unsigned_seed, {parameters}); other = {name}(reference, {parameters});
          ok &= value == other && unsigned_seed == reference[7:0];
          wide = 64'h1234567800000011; reference = 17;
          value = {name}(wide, {parameters}); other = {name}(reference, {parameters});
          extended = reference;
          ok &= value == other && wide == extended;
          wide = 64'hxxxxxxxx00000011; reference = 17;
          value = {name}(wide, {parameters}); other = {name}(reference, {parameters});
          extended = reference;
          ok &= value == other && wide === extended;
          unsigned_wide = 17; reference = 17;
          value = {name}(unsigned_wide, {parameters}); other = {name}(reference, {parameters});
          extended = reference;
          ok &= value == other && unsigned_wide == extended;
          masked = 32'hxxxxxxxx; reference = 0;
          value = {name}(masked, {parameters}); other = {name}(reference, {parameters});
          ok &= value == other && masked === reference && !$isunknown(masked);
          masked = 32'h0000x011; reference = 17;
          value = {name}(masked, {parameters}); other = {name}(reference, {parameters});
          ok &= value == other && masked === reference && !$isunknown(masked);
        "#
        ));
    }
    format!(
        r#"module Top(output bit ok);
      int seeds[0:1], calls, value, other, reference;
      logic signed [7:0] narrow;
      logic [7:0] unsigned_seed;
      logic [63:0] unsigned_wide;
      logic [31:0] masked;
      longint wide, extended;
      function automatic int index(); calls = calls + 1; return calls - 1; endfunction
      initial begin ok = 1; {body} end
    endmodule"#
    )
}

macro_rules! distribution_backends {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;
            #[test]
            fn distribution_int_formals_capture_and_masks() {
                let source = actuals_source();
                for four_state in [false, true] {
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(&source, Path::new("distribution_actuals.sv"))], "Top",
                    ).four_state(four_state).$backend().unwrap();
                    sim.run_until(0).unwrap();
                    assert_eq!(sim.get(sim.signal("ok")), 1u8.into(), "four_state={four_state}");
                }
            }

            #[test]
            fn distribution_rejects_invalid_positive_formals_without_copyout() {
                for (function, parameters) in [
                    ("$dist_exponential", "0"), ("$dist_poisson", "-1"),
                    ("$dist_chi_square", "0"), ("$dist_t", "-1"),
                    ("$dist_erlang", "0, 10"), ("$dist_erlang", "2, 0"),
                ] {
                    let source = format!(r#"module Top(output longint seed, output int value);
                      initial begin seed = 64'h1234567800000011;
                        value = {function}(seed, {parameters}); end
                    endmodule"#);
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(&source, Path::new("distribution_invalid.sv"))], "Top",
                    ).$backend().unwrap();
                    let error = sim.run_until(0).unwrap_err();
                    assert!(error.to_string().contains("requires positive distribution parameters"), "{error}");
                    assert_eq!(sim.get(sim.signal("seed")), 0x1234567800000011u64.into());
                }
            }

            #[test]
            fn distribution_checkpoint_replays_seed_and_return_slots() {
                let source = r#"module Top(output int value, seed);
                  timeunit 1ps; timeprecision 1ps;
                  initial seed = 17;
                  always begin #1; value = $dist_normal(seed, 0, 10); end
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("distribution_checkpoint.sv"))], "Top",
                ).$backend().unwrap();
                sim.run_until(0).unwrap();
                let checkpoint = sim.checkpoint().unwrap();
                sim.run_until(2).unwrap();
                let expected_seed = sim.get(sim.signal("seed"));
                let expected_value = sim.get(sim.signal("value"));
                sim.restore(&checkpoint).unwrap();
                sim.run_until(2).unwrap();
                assert_eq!(sim.get(sim.signal("seed")), expected_seed);
                assert_eq!(sim.get(sim.signal("value")), expected_value);
            }

            #[test]
            fn distribution_initial_requires_timed_simulation() {
                let source = r#"module Top(output int value);
                  int seed;
                  initial begin seed = 17; value = $dist_normal(seed, 0, 10); end
                endmodule"#;
                let error = celox::Simulator::from_sv_sources(
                    vec![(source, Path::new("distribution_initial.sv"))], "Top",
                ).$backend().err().expect("untimed initial distribution accepted");
                assert!(matches!(error.kind(), celox::SimulatorErrorKind::ProcessRequiresSimulation(_)), "{error}");
            }

            #[test]
            fn distribution_edge_requires_timed_simulation() {
                let source = r#"module Top(input bit clk, output int value);
                  int seed;
                  always_ff @(posedge clk) begin seed = 17; value = $dist_normal(seed, 0, 10); end
                endmodule"#;
                let error = celox::Simulator::from_sv_sources(
                    vec![(source, Path::new("distribution_edge.sv"))], "Top",
                ).$backend().err().expect("untimed distribution accepted");
                assert!(matches!(error.kind(), celox::SimulatorErrorKind::ProcessRequiresSimulation(_)), "{error}");
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("distribution_edge.sv"))], "Top",
                ).$backend().unwrap();
                sim.schedule("clk", 0, 1).unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(sim.get(sim.signal("value")), (u32::MAX - 3).into());
            }
        }
    };
}
distribution_backends!(interpreter, build_interpreter);
distribution_backends!(cranelift, build);
#[cfg(target_arch = "x86_64")]
distribution_backends!(native, build_native);
distribution_backends!(wasm, build_wasm);

#[test]
fn distribution_seed_is_an_integral_inout_variable() {
    for (name, parameters, _, _) in DISTRIBUTIONS {
        let source =
            format!("module Top; int value; initial value = {name}(17, {parameters}); endmodule");
        let error = celox::Simulation::from_sv_sources(
            vec![(&source, Path::new("distribution_seed.sv"))],
            "Top",
        )
        .build_interpreter()
        .expect_err("literal inout seed accepted");
        assert!(error.to_string().contains("output/inout"), "{error}");
    }
}
