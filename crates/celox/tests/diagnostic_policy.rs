use celox::{DiagnosticLevel, DiagnosticPolicy, Simulator, SimulatorErrorKind};

fn policy(code: &str, level: DiagnosticLevel) -> DiagnosticPolicy {
    let mut policy = DiagnosticPolicy::default();
    policy.set(code, level);
    policy
}

const VERYL_WARNING: &str = r#"
module Top (tmp: output logic<8>, out: output logic) {
    function write_outputs(first: output logic<8>, second: output logic<8>) -> logic {
        first = 8'd1;
        second = 8'd2;
        return 1'b1;
    }
    always_comb {
        tmp = 0;
        out = write_outputs(second: tmp, first: tmp) & 1'b1;
    }
}
"#;

#[test]
fn existing_warnings_can_be_suppressed_or_promoted() {
    let code = "unspecified_output_copy_order";
    let sim = Simulator::builder(VERYL_WARNING, "Top").build().unwrap();
    assert_eq!(sim.warnings().len(), 1);
    let sim = Simulator::builder(VERYL_WARNING, "Top")
        .diagnostic_policy(policy(code, DiagnosticLevel::Allow))
        .build()
        .unwrap();
    assert!(sim.warnings().is_empty());
    let error = Simulator::builder(VERYL_WARNING, "Top")
        .diagnostic_policy(policy(code, DiagnosticLevel::Error))
        .build()
        .err()
        .unwrap();
    assert!(matches!(
        error.kind(),
        SimulatorErrorKind::DiagnosticPolicy(_)
    ));
    assert!(error.to_string().contains(code));
}

#[test]
fn invalid_overrides_and_config_levels_fail() {
    for value in [
        "mixed_timescale",
        "=warn",
        "mixed_timescale=quiet",
        "foo-bar=allow",
    ] {
        assert!(
            DiagnosticPolicy::default().set_override(value).is_err(),
            "{value}"
        );
    }
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("celox.toml");
    std::fs::write(&config, "[diagnostics]\nmixed_timescale = 'quiet'\n").unwrap();
    assert!(
        DiagnosticPolicy::from_file(&config)
            .unwrap_err()
            .contains("quiet")
    );
}

#[cfg(feature = "systemverilog")]
mod sv {
    use super::*;
    use std::path::Path;

    const PLAIN: &str = "module Plain(output logic ok); assign ok = $timeunit() == -9 && $timeprecision() == -12; endmodule";
    const EXPLICIT: &str = "module Top(output logic ok); timeunit 10ns / 1ns; wire child_ok; Plain child(child_ok); assign ok = child_ok && $timeunit() == -8 && $timeprecision() == -9 && $timeprecision($root) == -12; endmodule";

    fn builder() -> celox::SimulatorBuilder<'static> {
        Simulator::from_sv_sources(
            vec![
                (PLAIN, Path::new("plain.sv")),
                (EXPLICIT, Path::new("top.sv")),
            ],
            "Top",
        )
    }

    #[test]
    fn mixed_timescales_remain_an_error_by_default() {
        let error = builder().build().err().unwrap().to_string();
        assert!(error.contains("mixed explicit and default"), "{error}");
    }

    macro_rules! relaxed_backend {
        ($name:ident, $method:ident) => {
            #[test]
            fn $name() {
                for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
                    let mut sim = builder()
                        .diagnostic_policy(policy("mixed_timescale", level))
                        .$method()
                        .unwrap();
                    assert_eq!(
                        sim.warnings().len(),
                        usize::from(level == DiagnosticLevel::Warn)
                    );
                    if let Some(warning) = sim.warnings().first() {
                        assert!(celox::render_diagnostic(warning).contains("mixed_timescale"));
                    }
                    sim.eval_comb().unwrap();
                    assert_eq!(sim.get(sim.signal("ok")), 1u8.into());
                }
            }
        };
    }
    relaxed_backend!(native_retains_timing_semantics, build_native);
    relaxed_backend!(cranelift_retains_timing_semantics, build_cranelift);
    relaxed_backend!(interpreter_retains_timing_semantics, build_interpreter);
    relaxed_backend!(wasm_retains_timing_semantics, build_wasm);

    #[test]
    fn suppressing_an_sv_warning_does_not_hide_a_different_error() {
        let source = "package P; function automatic int f(); $exit; return 2; endfunction localparam int N = f(); localparam int BAD = missing(); endpackage module Top(output int y); assign y = P::N; endmodule";
        let error = Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
            .diagnostic_policy(policy("exit_outside_program", DiagnosticLevel::Allow))
            .build()
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("user constant function call"),
            "{error}"
        );
        assert!(error.warnings().is_empty(), "{error}");
    }

    #[test]
    fn mixed_language_interface_probes_use_the_same_policy() {
        let veryl = "module Top(a: input logic<8>, y: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; }";
        let explicit = "module Unused; timeunit 1ns / 1ps; endmodule";
        let bus = "interface Bus; logic [7:0] req, rsp; assign rsp = req + 1; endinterface";
        let mut sim = Simulator::from_mixed_sources(
            vec![(veryl, Path::new("top.veryl"))],
            vec![
                (bus, Path::new("bus.sv")),
                (explicit, Path::new("explicit.sv")),
            ],
            "Top",
        )
        .diagnostic_policy(policy("mixed_timescale", DiagnosticLevel::Warn))
        .build()
        .unwrap();
        assert_eq!(
            sim.warnings()
                .iter()
                .filter(|warning| matches!(
                    warning,
                    celox::CompilationWarning::SystemVerilog(celox::SvFrontendWarning {
                        kind: celox::SvFrontendWarningKind::MixedTimescale,
                        ..
                    })
                ))
                .count(),
            1
        );
        let a = sim.signal("a");
        sim.modify(|io| io.set(a, 8u8)).unwrap();
        assert_eq!(sim.get(sim.signal("y")), 9u8.into());
    }

    #[test]
    fn relaxed_timescales_still_reject_invalid_precision() {
        let source = "module Top; timeunit 1ps / 1ns; endmodule";
        assert!(
            Simulator::from_sv_sources(vec![(source, Path::new("top.sv"))], "Top")
                .diagnostic_policy(policy("mixed_timescale", DiagnosticLevel::Allow))
                .build()
                .is_err()
        );
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn overlapping_always_ff_memories_require_explicit_relaxation() {
    const SOURCE: &str = r#"module Top(input bit a, b, input bit[1:0] x, y,
      output wire[7:0] q);
      logic[7:0] mem[4];
      always_ff @(posedge a) mem[x] <= 8'h12;
      always_ff @(posedge b) mem[y] <= 8'h34;
      assign q=mem[0];
    endmodule"#;
    let build = |level| {
        celox::Simulation::from_sv_sources(
            vec![(SOURCE, std::path::Path::new("shared_ff_memory.sv"))],
            "Top",
        )
        .diagnostic_policy(policy("multiple_always_ff_drivers", level))
        .build_cranelift()
    };
    let error = build(DiagnosticLevel::Error).err().unwrap().to_string();
    assert!(error.contains("multiple variable drivers"), "{error}");
    for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
        let sim = build(level).unwrap();
        assert_eq!(
            sim.warnings().len(),
            usize::from(level == DiagnosticLevel::Warn)
        );
    }
    let error = celox::Simulation::from_sv_sources(
        vec![(SOURCE, std::path::Path::new("shared_ff_memory.sv"))],
        "Top",
    )
    .build_cranelift()
    .err()
    .unwrap()
    .to_string();
    assert!(error.contains("multiple variable drivers"), "{error}");
}
