#![cfg(feature = "systemverilog")]
use std::path::Path;

macro_rules! ordinary_host_requests {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            #[test]
            fn ordinary_edge_host_calls_require_timed_simulation() {
                for body in [
                    "q = q + 1; #0; enabled = $test$plusargs(\"ON\"); q = q + 1;",
                    "q = q + 1; enabled = $test$plusargs(\"ON\"); q = q + 1;",
                    "q = q + 1; $fwrite(1, \"edge\"); q = q + 1;",
                    "q = q + 1; enabled = $fopen(\"unused.txt\"); q = q + 1;",
                ] {
                    let source = format!(
                        "module Top(input bit clk, output int q, enabled); always @(posedge clk) begin {body} end endmodule"
                    );
                    let error = celox::Simulator::from_sv_sources(
                        vec![(&source, Path::new("ordinary_host.sv"))], "Top",
                    ).$backend().err().expect("untimed edge host process was accepted");
                    assert!(matches!(error.kind(), celox::SimulatorErrorKind::ProcessRequiresSimulation(_)), "{error}");
                }
            }

            #[test]
            fn ordinary_edge_host_and_surrounding_statements_run_on_each_edge() {
                let source = r#"module Top(input bit clk, output int q, enabled, reference);
                  always_ff @(posedge clk) reference <= reference + 1;
                  always @(posedge clk) begin
                    q = q + 1;
                    #0;
                    enabled = $test$plusargs("ON");
                    $fwrite(1, "edge");
                    q = q + 1;
                  end
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("ordinary_host.sv"))], "Top",
                ).plusargs(["+ON"]).$backend().unwrap();
                sim.schedule("clk", 1, 1).unwrap();
                sim.schedule("clk", 2, 0).unwrap();
                sim.schedule("clk", 3, 1).unwrap();
                sim.run_until(3).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 4u8.into());
                assert_eq!(sim.get(sim.signal("reference")), 2u8.into());
                assert_eq!(sim.get(sim.signal("enabled")), 1u8.into());
                assert_eq!(sim.drain_runtime_events(), vec![
                    celox::RuntimeEvent::Write { message: b"edge".to_vec() },
                    celox::RuntimeEvent::Write { message: b"edge".to_vec() },
                ]);
            }

            #[test]
            fn repeating_host_requirement_does_not_bootstrap_an_immediate_body() {
                let source = r#"module Top(output int count, enabled);
                  timeunit 1ps; timeprecision 1ps;
                  always begin
                    count = count + 1;
                    #1;
                    enabled = $test$plusargs("ON");
                  end
                endmodule"#;
                let error = celox::Simulator::from_sv_sources(
                    vec![(source, Path::new("delayed_host.sv"))], "Top",
                ).$backend().err().expect("untimed repeating host process was accepted");
                assert!(matches!(error.kind(), celox::SimulatorErrorKind::ProcessRequiresSimulation(_)), "{error}");
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("delayed_host.sv"))], "Top",
                ).plusargs(["+ON"]).$backend().unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(sim.get(sim.signal("count")), 1u8.into());
                assert_eq!(sim.get(sim.signal("enabled")), 0u8.into());
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("count")), 2u8.into());
                assert_eq!(sim.get(sim.signal("enabled")), 1u8.into());
            }

            #[test]
            fn one_shot_initial_host_requires_simulation() {
                let source = r#"module Top(output int enabled);
                  initial enabled = $test$plusargs("ON");
                endmodule"#;
                let error = celox::Simulator::from_sv_sources(
                    vec![(source, Path::new("initial_host.sv"))], "Top",
                ).$backend().err().expect("untimed initial host process was accepted");
                assert!(matches!(error.kind(), celox::SimulatorErrorKind::ProcessRequiresSimulation(_)), "{error}");
            }
        }
    };
}
ordinary_host_requests!(interpreter, build_interpreter);
ordinary_host_requests!(cranelift, build);
#[cfg(target_arch = "x86_64")]
ordinary_host_requests!(native, build_native);
ordinary_host_requests!(wasm, build_wasm);
