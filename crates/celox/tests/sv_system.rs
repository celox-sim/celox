#![cfg(all(feature = "systemverilog", unix))]

use std::path::Path;

#[test]
fn invalid_command_arguments_are_rejected() {
    for call in ["$system(\"a\", \"b\")", "$system(,)", "$system(1.5)"] {
        let source = format!("module Top; initial {call}; endmodule");
        assert!(
            celox::Simulation::from_sv_sources(vec![(&source, Path::new("invalid.sv"))], "Top")
                .build_interpreter()
                .is_err(),
            "accepted {call}"
        );
    }
}

// Shell commands and their effects are host-specific API behavior, so these
// regressions live outside the portable language corpus.
macro_rules! system_tests {
    ($module:ident, $backend:ident) => {
        mod $module {
            use super::*;

            #[test]
            fn declaration_and_clocked_subroutine_calls() {
                let source = r#"
                    module Top(input bit clk, output int startup, output int status);
                      int declared = $system("exit 4");
                      assign startup = declared;
                      function int command();
                        command = $system("exit 5");
                      endfunction
                      always @(posedge clk) status <= command();
                    endmodule
                "#;
                let mut sim = celox::Simulation::from_sv_sources(
                    vec![(source, Path::new("system_calls.sv"))],
                    "Top",
                )
                .$backend()
                .unwrap();
                sim.run_until(0).unwrap();
                assert_eq!(sim.get(sim.signal("startup")), 1024u32.into());
                sim.schedule("clk", 1, 1).unwrap();
                sim.run_until(1).unwrap();
                assert_eq!(sim.get(sim.signal("status")), 1280u32.into());
            }

            #[test]
            fn commands_execute_once_in_order_at_runtime() {
                for four_state in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("effects");
                    let source = format!(
                        r#"
                        module Top(output int status, output bit ok);
                          timeunit 1ps; timeprecision 1ps;
                          string command;
                          initial begin
                            command = "printf a >> '{}'";
                            #1;
                            $system(command);
                            status = $system("printf b >> '{}'; exit 7");
                            ok = status == 1792 && $bits($system("exit 99")) == 32;
                            if (0 && $system("printf bad >> '{}'")) ok = 0;
                            repeat (2) $system("printf c >> '{}'");
                            #1;
                            status = $system("");
                            ok &= status == 0;
                            status = $system();
                            ok &= status != 0;
                            $system;
                          end
                        endmodule
                    "#,
                        path.display(),
                        path.display(),
                        path.display(),
                        path.display()
                    );
                    let mut sim = celox::Simulation::from_sv_sources(
                        vec![(&source, Path::new("system.sv"))],
                        "Top",
                    )
                    .four_state(four_state)
                    .$backend()
                    .unwrap();
                    assert!(!path.exists(), "building must not execute commands");
                    sim.run_until(0).unwrap();
                    assert!(!path.exists(), "delayed commands ran too early");
                    sim.run_until(1).unwrap();
                    assert_eq!(std::fs::read(&path).unwrap(), b"abcc");
                    assert_eq!(sim.get(sim.signal("status")), 1792u32.into());
                    sim.run_until(2).unwrap();
                    assert_ne!(sim.get(sim.signal("status")), 0u8.into());
                    assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
                    assert_eq!(std::fs::read(&path).unwrap(), b"abcc");
                    let state = sim.save_state().unwrap();
                    let error = sim.load_state(&state).unwrap_err();
                    assert!(error.to_string().contains("external"), "{error}");
                }
            }
        }
    };
}

system_tests!(interpreter, build_interpreter);
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
system_tests!(native, build_native);
#[cfg(feature = "cranelift")]
system_tests!(cranelift, build_cranelift);
#[cfg(feature = "wasm-runtime")]
system_tests!(wasm, build_wasm);
