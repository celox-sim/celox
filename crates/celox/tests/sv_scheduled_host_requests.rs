#![cfg(feature = "systemverilog")]
use std::path::Path;

macro_rules! scheduled_requests {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;
            #[test]
            fn failed_mcd_open_cannot_be_silently_closed() {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("missing").join("output.txt");
                let source = format!(
                    r#"module Top;
                  int fd;
                  initial begin fd = $fopen("{}"); $fclose(fd); end
                endmodule"#,
                    path.display()
                );
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(&source, Path::new("close_zero_mcd.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                let error = sim
                    .run_until(0)
                    .expect_err("closing zero MCD must be invalid");
                assert!(error.to_string().contains("invalid or closed"), "{error}");
            }

            #[test]
            fn disjoint_scheduled_and_regular_writers_remain_valid() {
                let source = r#"module Top(input bit clk, output logic [1:0] q);
                  always_ff @(posedge clk) q[0] <= 1;
                  always_ff @(posedge clk) q[1] <= $test$plusargs("ON");
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("disjoint_scheduled_drivers.sv"))],
                    "Top",
                )
                .plusargs(["+ON"])
                .$backend()
                .unwrap();
                sim.schedule("clk", 1, 1).unwrap();
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 3u8.into());
            }

            #[test]
            fn scheduled_writers_participate_in_all_driver_checks() {
                for source in [
                    r#"module Top(input bit clk, output bit q);
                      always_ff @(posedge clk) begin q <= 1; $fwrite(1, "host"); end
                      always_ff @(posedge clk) q <= 0;
                    endmodule"#,
                    r#"module Top(input bit clk, output bit q);
                      always_ff @(posedge clk) begin q <= 1; $fwrite(1, "first"); end
                      always_ff @(posedge clk) begin q <= 0; $fwrite(1, "second"); end
                    endmodule"#,
                    r#"module Top(input bit clk, output bit q);
                      always_ff @(posedge clk) begin q <= 1; $fwrite(1, "host"); end
                      always_comb q = 0;
                    endmodule"#,
                    r#"module Child(output bit q); always_comb q = 0; endmodule
                    module Top(input bit clk, output bit q);
                      Child child(q);
                      always_ff @(posedge clk) begin q <= 1; $fwrite(1, "host"); end
                    endmodule"#,
                    r#"package p; bit state; endpackage
                    module Writer(input bit clk);
                      import p::*;
                      always_ff @(posedge clk) begin state <= 1; $fwrite(1, "host"); end
                    endmodule
                    module Top(input bit clk, output bit q);
                      import p::*;
                      Writer a(clk), b(clk);
                      assign q = state;
                    endmodule"#,
                ] {
                    let error = celox::Simulation::from_sv_sources(
                        vec![(source, Path::new("scheduled_drivers.sv"))],
                        "Top",
                    )
                    .$backend()
                    .err()
                    .expect("conflicting scheduled writer was accepted");
                    let message = error.to_string();
                    assert!(
                        message.contains("multiple variable drivers")
                            || message.contains("multiple net drivers")
                            || message.contains("multiple drivers of package variable"),
                        "{message}"
                    );
                }
            }

            #[test]
            fn failed_mcd_open_cannot_silently_discard_output() {
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("missing").join("output.txt");
                for operation in ["$fwrite", "$fdisplay"] {
                    let source = format!(
                        r#"module Top;
                      int fd;
                      initial begin fd = $fopen("{}"); {operation}(fd, "lost"); end
                    endmodule"#,
                        path.display()
                    );
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(&source, Path::new("zero_mcd.sv"))],
                        "Top",
                    )
                    .$backend()
                    .unwrap();
                    let error = sim.run_until(0).expect_err("zero MCD must be invalid");
                    assert!(error.to_string().contains("invalid or closed"), "{error}");
                    assert!(!path.exists());
                }
            }

            #[test]
            fn a_scheduled_clock_keeps_priority_over_a_retained_reset() {
                let source = r#"
                module Top(input bit clk, input bit shared, output bit regular, host);
                  always_ff @(posedge clk or negedge shared)
                    if (!shared) regular <= 0; else regular <= 1;
                  always_ff @(posedge shared) host <= $test$plusargs("ON");
                endmodule
                "#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("shared_edge_roles.sv"))],
                    "Top",
                )
                .plusargs(["+ON"])
                .$backend()
                .unwrap();
                let signals = sim.named_signals();
                assert_eq!(
                    signals
                        .iter()
                        .find(|s| s.name == "shared")
                        .unwrap()
                        .info
                        .type_kind,
                    celox_design::PortTypeKind::Clock
                );
                sim.schedule("shared", 1, 1).unwrap();
                sim.schedule("clk", 2, 1).unwrap();
                sim.schedule("shared", 3, 0).unwrap();
                sim.run_until(2).unwrap();
                assert_eq!(sim.get(sim.signal("host")), 1u8.into());
                assert_eq!(sim.get(sim.signal("regular")), 1u8.into());
                sim.run_until(3).unwrap();
                assert_eq!(sim.get(sim.signal("regular")), 0u8.into());
            }

            #[test]
            fn scheduled_clock_and_reset_widths_are_validated() {
                for source in [
                    r#"module Top(input bit [1:0] clk, output bit q);
                      always_ff @(posedge clk) q <= $test$plusargs("ON");
                    endmodule"#,
                    r#"module Top(input bit clk, input bit [1:0] rst_n, output bit q);
                      always_ff @(posedge clk or negedge rst_n)
                        if (!rst_n) q <= 0; else q <= $test$plusargs("ON");
                    endmodule"#,
                ] {
                    let error = celox::Simulation::from_sv_sources(
                        vec![(source, Path::new("wide_host_event.sv"))],
                        "Top",
                    )
                    .$backend()
                    .err()
                    .expect("multi-bit event must be rejected");
                    assert!(
                        error
                            .to_string()
                            .contains("multi-bit always_ff event signal"),
                        "{error}"
                    );
                }
            }

            #[test]
            fn stdout_is_preserved_when_another_mcd_channel_fails() {
                let source = r#"module Top;
                  initial $fwrite(3, "stdout survives");
                endmodule"#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("mcd_stdout_failure.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                assert!(sim.run_until(0).is_err());
                assert_eq!(
                    sim.drain_runtime_events(),
                    vec![celox::RuntimeEvent::Write {
                        message: b"stdout survives".to_vec()
                    }]
                );
            }

            #[test]
            fn omitted_defaults_schedule_requests_for_each_call_shape() {
                let source = r#"
                module Top(input bit clk, output int q);
                  function automatic int enabled(input int value = $test$plusargs("ON"));
                    return value;
                  endfunction
                  always_ff @(posedge clk) begin
                    q = enabled(0);
                    q = enabled();
                  end
                endmodule
                "#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("default_host.sv"))],
                    "Top",
                )
                .plusargs(["+ON"])
                .$backend()
                .unwrap();
                sim.schedule("clk", 1, 1).unwrap();
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            }

            #[test]
            fn folded_host_request_keeps_the_scheduled_process_requirement() {
                let source = r#"
                module Top(input bit clk, input bit d, output bit q);
                  always_ff @(posedge clk) begin
                    if (1'bx) $fwrite(1, "dead");
                    q <= d;
                  end
                endmodule
                "#;
                let result = celox::Simulator::from_sv_sources(
                    vec![(source, Path::new("dead_host.sv"))],
                    "Top",
                )
                .$backend();
                assert!(
                    result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("require Simulation")
                );
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("dead_host.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.checkpoint().unwrap(); // The folded call leaves no file-resource site.
                let d = sim.signal("d");
                sim.modify(|io| io.set(d, 1u8)).unwrap();
                sim.schedule("clk", 1, 1).unwrap();
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 1u8.into());
            }

            #[test]
            fn scheduled_edges_preserve_clock_and_reset_types() {
                let source = r#"
                module Top(input bit clk, input bit rst_n, output bit q);
                  always_ff @(posedge clk or negedge rst_n) begin
                    if (!rst_n) q <= 0;
                    else q <= $test$plusargs("ON");
                  end
                endmodule
                "#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("reset_host.sv"))],
                    "Top",
                )
                .plusargs(["+ON"])
                .$backend()
                .unwrap();
                let signals = sim.named_signals();
                let clock = signals.iter().find(|s| s.name == "clk").unwrap();
                let reset = signals.iter().find(|s| s.name == "rst_n").unwrap();
                assert_eq!(clock.info.type_kind, celox_design::PortTypeKind::Clock);
                assert_eq!(
                    reset.info.type_kind,
                    celox_design::PortTypeKind::ResetAsyncLow
                );
                sim.schedule("rst_n", 0, 1).unwrap();
                sim.schedule("clk", 1, 1).unwrap();
                sim.schedule("rst_n", 2, 0).unwrap();
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 1u8.into());
                sim.run_until(2).unwrap();
                assert_eq!(sim.get(sim.signal("q")), 0u8.into());
            }
        }
    };
}
scheduled_requests!(interpreter, build_interpreter);
scheduled_requests!(cranelift, build_cranelift);
scheduled_requests!(wasm, build_wasm);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
scheduled_requests!(native, build_native);
