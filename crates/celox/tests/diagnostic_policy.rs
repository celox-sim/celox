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

mod inferred_latch {
    use super::*;
    use miette::Diagnostic;
    use std::path::Path;

    const VERYL: &str = "module Top(en: input logic, d: input logic<8>, q: output logic<8>, t: output logic<8>) { always_comb { t = d; if en { q = d; } } }";
    #[cfg(feature = "systemverilog")]
    const SV: &str = "module Top(input logic en, input logic [7:0] d, output logic [7:0] q, t); always_comb begin t=d; if(en) q=d; end endmodule";

    fn veryl() -> celox::SimulatorBuilder<'static> {
        Simulator::builder(VERYL, "Top")
    }

    fn assert_behavior<B: celox::SimBackend>(mut sim: Simulator<B>, level: DiagnosticLevel) {
        assert_eq!(
            sim.warnings().len(),
            usize::from(level == DiagnosticLevel::Warn),
            "{:?}",
            sim.warnings()
        );
        if let Some(warning) = sim.warnings().first() {
            assert_eq!(warning.code().unwrap().to_string(), "inferred_latch");
            let rendered = celox::render_diagnostic(warning);
            assert!(rendered.contains("if"), "{rendered}");
        }
        let en = sim.signal("en");
        let d = sim.signal("d");
        let q = sim.signal("q");
        sim.modify(|io| {
            io.set(en, 1u8);
            io.set(d, 7u8);
        })
        .unwrap();
        assert_eq!(sim.get(q), 7u8.into());
        sim.modify(|io| io.set(d, 13u8)).unwrap();
        assert_eq!(sim.get(q), 13u8.into());
        sim.modify(|io| {
            io.set(en, 0u8);
            io.set(d, 99u8);
        })
        .unwrap();
        sim.eval_comb().unwrap();
        assert_eq!(sim.get(q), 13u8.into());
    }

    macro_rules! backend {
        ($name:ident, $method:ident) => {
            #[test]
            fn $name() {
                for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
                    assert_behavior(
                        veryl()
                            .diagnostic_policy(policy("inferred_latch", level))
                            .$method()
                            .unwrap(),
                        level,
                    );
                    #[cfg(feature = "systemverilog")]
                    assert_behavior(
                        Simulator::from_sv_sources(vec![(SV, Path::new("latch.sv"))], "Top")
                            .diagnostic_policy(policy("inferred_latch", level))
                            .$method()
                            .unwrap(),
                        level,
                    );
                }
            }
        };
    }
    backend!(interpreter, build_interpreter);
    backend!(native, build_native);
    backend!(cranelift, build_cranelift);
    backend!(wasm, build_wasm);
    backend!(tiered, build_tiered);

    #[test]
    fn default_and_explicit_error_report_the_diagnostic_code() {
        for level in [None, Some(DiagnosticLevel::Error)] {
            let mut builder = veryl();
            if let Some(level) = level {
                builder = builder.diagnostic_policy(policy("inferred_latch", level));
            }
            let error = builder.build_interpreter().unwrap_err();
            assert!(
                matches!(error.kind(), SimulatorErrorKind::DiagnosticPolicy(_)),
                "{error}"
            );
            assert!(error.to_string().contains("inferred_latch"), "{error}");
            #[cfg(feature = "systemverilog")]
            {
                let error = Simulator::from_sv_sources(vec![(SV, Path::new("latch.sv"))], "Top")
                    .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Error))
                    .build_interpreter()
                    .unwrap_err();
                assert!(error.to_string().contains("inferred_latch"), "{error}");
            }
        }
    }

    #[test]
    fn explicit_policy_wins_over_shorthand_in_both_setter_orders() {
        for level in [
            DiagnosticLevel::Error,
            DiagnosticLevel::Warn,
            DiagnosticLevel::Allow,
        ] {
            for flag in [false, true] {
                for policy_first in [false, true] {
                    let builder = if policy_first {
                        veryl()
                            .diagnostic_policy(policy("inferred_latch", level))
                            .allow_inferred_latches(flag)
                    } else {
                        veryl()
                            .allow_inferred_latches(flag)
                            .diagnostic_policy(policy("inferred_latch", level))
                    };
                    let result = builder.build_interpreter();
                    if level == DiagnosticLevel::Error {
                        assert!(result.unwrap_err().to_string().contains("inferred_latch"));
                    } else {
                        assert_behavior(result.unwrap(), level);
                    }
                }
            }
        }
        assert_behavior(
            veryl()
                .allow_inferred_latches(true)
                .build_interpreter()
                .unwrap(),
            DiagnosticLevel::Allow,
        );
    }

    #[test]
    fn complete_combinational_assignments_do_not_warn() {
        let source = "module Top(en: input logic, d: input logic<8>, q: output logic<8>) { always_comb { if en { q = d; } else { q = 0; } } }";
        let sim = Simulator::builder(source, "Top")
            .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Warn))
            .build_interpreter()
            .unwrap();
        assert!(sim.warnings().is_empty(), "{:?}", sim.warnings());
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn explicit_and_wildcard_latches_do_not_emit_inferred_diagnostics() {
        for keyword in ["always_latch", "always @*"] {
            let source = format!(
                "module Top(input logic en,d, output logic q); {keyword} if(en) q=d; endmodule"
            );
            let sim =
                Simulator::from_sv_sources(vec![(source.as_str(), Path::new("latch.sv"))], "Top")
                    .build_interpreter()
                    .unwrap();
            assert!(sim.warnings().is_empty());
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn relaxation_does_not_hide_nba_or_continuous_feedback_errors() {
        for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
            for source in [
                "module Top(input logic en,d, output logic q); always_latch if(en) q<=d; endmodule",
                "module Top(input logic en, output logic q); assign q=q^en; endmodule",
            ] {
                assert!(
                    Simulator::from_sv_sources(vec![(source, Path::new("bad.sv"))], "Top")
                        .diagnostic_policy(policy("inferred_latch", level))
                        .build_interpreter()
                        .is_err()
                );
            }
        }
    }
    #[cfg(feature = "systemverilog")]
    #[test]
    fn state_reading_processes_use_the_same_latch_diagnostic() {
        let source = "module Top(input logic en, output logic [7:0] q); always_comb if(en) q=$time; endmodule";
        let builder =
            || Simulator::from_sv_sources(vec![(source, Path::new("state_latch.sv"))], "Top");
        assert!(
            builder()
                .build_interpreter()
                .unwrap_err()
                .to_string()
                .contains("inferred_latch")
        );
        for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
            let sim = builder()
                .diagnostic_policy(policy("inferred_latch", level))
                .build_interpreter()
                .unwrap();
            assert_eq!(
                sim.warnings().len(),
                usize::from(level == DiagnosticLevel::Warn)
            );
        }
        for body in [
            "q=$time;",
            "if(en) q=$time; else q=0;",
            "if(1) q=$time;",
            "q[3:0]=$time; q[7:4]=$time;",
        ] {
            let source = format!(
                "module Top(input logic en, output logic [7:0] q); always_comb begin {body} end endmodule"
            );
            let sim = Simulator::from_sv_sources(
                vec![(source.as_str(), Path::new("complete_state.sv"))],
                "Top",
            )
            .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Warn))
            .build_interpreter()
            .unwrap();
            assert!(sim.warnings().is_empty(), "{body}: {:?}", sim.warnings());
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn static_block_locals_participate_in_latch_diagnostics() {
        for body in [
            "int t; if(a) t=5; y=a ? t : 0;",
            // State-reading processes bypass symbolic comb lowering entirely.
            "int t; if(a) t=5; y=$time+t;",
        ] {
            let source = format!(
                "module Top(input logic a, output logic [7:0] y); always_comb begin {body} end endmodule"
            );
            let builder = || {
                Simulator::from_sv_sources(
                    vec![(source.as_str(), Path::new("static_latch.sv"))],
                    "Top",
                )
            };
            assert!(
                builder()
                    .build_interpreter()
                    .expect_err(body)
                    .to_string()
                    .contains("inferred_latch"),
                "{body}"
            );
            for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
                let sim = builder()
                    .diagnostic_policy(policy("inferred_latch", level))
                    .build_interpreter()
                    .unwrap();
                assert_eq!(
                    sim.warnings().len(),
                    usize::from(level == DiagnosticLevel::Warn),
                    "{body}: {:?}",
                    sim.warnings()
                );
            }
        }
        for body in [
            // A static local whose entry value is never observed is safely
            // represented as an initialized activation-local temporary.
            "int t; if(a) t=5; y=$time;",
            "automatic int t; if(a) t=5; y=a ? t : 0;",
            "automatic int t; if(a) t=5; y=$time;",
            "int t; if(a) t=5; else t=0; y=$time+t;",
        ] {
            let source = format!(
                "module Top(input logic a, output logic [7:0] y); always_comb begin {body} end endmodule"
            );
            let sim = Simulator::from_sv_sources(
                vec![(source.as_str(), Path::new("complete_local.sv"))],
                "Top",
            )
            .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Warn))
            .build_interpreter()
            .unwrap();
            assert!(sim.warnings().is_empty(), "{body}: {:?}", sim.warnings());
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn complete_nonzero_lsb_state_reading_slice_does_not_warn() {
        let source = "module Top(input logic en, output logic [7:0] q); always_comb if(en) q[7:4]=$time; else q[7:4]=0; endmodule";
        for level in [DiagnosticLevel::Error, DiagnosticLevel::Warn] {
            let sim =
                Simulator::from_sv_sources(vec![(source, Path::new("complete_slice.sv"))], "Top")
                    .diagnostic_policy(policy("inferred_latch", level))
                    .build_interpreter()
                    .unwrap();
            assert!(sim.warnings().is_empty(), "{:?}", sim.warnings());
        }
    }

    // IEEE 1800-2023 9.2.2.2.1 excludes written expressions from implicit
    // sensitivity. A procedural read of previous q is state, not an endlessly
    // retriggering continuous-assignment feedback loop.
    #[cfg(feature = "systemverilog")]
    fn assert_complete_read_before_write_activation<B: celox::SimBackend>(
        mut sim: Simulator<B>,
        level: DiagnosticLevel,
    ) {
        assert_eq!(
            sim.warnings().len(),
            usize::from(level == DiagnosticLevel::Warn)
        );
        let en = sim.signal("en");
        let q = sim.signal("q");
        for (input, expected) in [(0u8, 0u8), (1, 1), (0, 1), (1, 0)] {
            sim.modify(|io| io.set(en, input)).unwrap();
            assert_eq!(sim.get(q), expected.into());
            sim.eval_comb().unwrap();
            sim.eval_comb().unwrap();
            assert_eq!(sim.get(q), expected.into());
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn complete_procedural_read_before_write_is_recoverable_state() {
        let source = "module Top(input logic en, output logic q); always_comb q=q^en; endmodule";
        let builder =
            || Simulator::from_sv_sources(vec![(source, Path::new("procedural_state.sv"))], "Top");
        let error = builder().build_interpreter().unwrap_err().to_string();
        assert!(error.contains("inferred_latch"), "{error}");
        for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
            assert_complete_read_before_write_activation(
                builder()
                    .diagnostic_policy(policy("inferred_latch", level))
                    .build_interpreter()
                    .unwrap(),
                level,
            );
            assert_complete_read_before_write_activation(
                builder()
                    .diagnostic_policy(policy("inferred_latch", level))
                    .build_native()
                    .unwrap(),
                level,
            );
            assert_complete_read_before_write_activation(
                builder()
                    .diagnostic_policy(policy("inferred_latch", level))
                    .build_cranelift()
                    .unwrap(),
                level,
            );
            assert_complete_read_before_write_activation(
                builder()
                    .diagnostic_policy(policy("inferred_latch", level))
                    .build_wasm()
                    .unwrap(),
                level,
            );
        }
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn complete_state_reading_array_assignments_do_not_warn() {
        let source = "module Top(output logic [7:0] q); logic [7:0] a[2]; always_comb begin a[0]=$time; a[1]=$time; end assign q=a[0]; endmodule";
        let sim = Simulator::from_sv_sources(vec![(source, Path::new("state_array.sv"))], "Top")
            .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Warn))
            .build_interpreter()
            .unwrap();
        assert!(sim.warnings().is_empty(), "{:?}", sim.warnings());
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn an_authored_process_warns_once_across_instances() {
        let source = "module Latch(input logic en,d, output logic q); always_comb if(en) q=d; endmodule module Top(input logic en,d, output logic a,b); Latch u(en,d,a); Latch v(en,d,b); endmodule";
        let sim = Simulator::from_sv_sources(vec![(source, Path::new("instances.sv"))], "Top")
            .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Warn))
            .build_interpreter()
            .unwrap();
        assert_eq!(sim.warnings().len(), 1, "{:?}", sim.warnings());
        let source = "module Latch(en: input logic, d: input logic, q: output logic) { always_comb { if en { q = d; } } } module Top(en: input logic, d: input logic, a: output logic, b: output logic) { inst u: Latch(en,d,q:a); inst v: Latch(en,d,q:b); }";
        let sim = Simulator::builder(source, "Top")
            .diagnostic_policy(policy("inferred_latch", DiagnosticLevel::Warn))
            .build_interpreter()
            .unwrap();
        assert_eq!(sim.warnings().len(), 1, "{:?}", sim.warnings());
    }

    #[cfg(feature = "systemverilog")]
    #[test]
    fn out_of_bounds_state_writes_do_not_expand_diagnostic_masks() {
        let source = "module Top(output logic [7:0] q); logic [7:0] a[2]; always_comb begin q=$time; a[-1]=$time; a[2147483647]=$time; end endmodule";
        // Invalid unpacked writes are ignored (IEEE 1800-2023 7.4.5).
        // Their offsets must not enlarge coverage masks or create a latch.
        for level in [DiagnosticLevel::Error, DiagnosticLevel::Warn] {
            let sim = Simulator::from_sv_sources(vec![(source, Path::new("oob_state.sv"))], "Top")
                .diagnostic_policy(policy("inferred_latch", level))
                .build_interpreter()
                .unwrap();
            assert!(sim.warnings().is_empty(), "{:?}", sim.warnings());
        }
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn interface_array_queries_require_a_recoverable_diagnostic_override() {
    use std::path::Path;
    let source = "interface Lane; logic v; endinterface module Top(output wire [7:0] y); Lane lanes[3:2](); assign y=$size(lanes); endmodule";
    let build =
        || Simulator::from_sv_sources(vec![(source, Path::new("size_extension.sv"))], "Top");
    let error = build().build_cranelift().err().unwrap();
    assert!(
        matches!(error.kind(), SimulatorErrorKind::DiagnosticPolicy(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("interface_array_query"),
        "{error}"
    );
    for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
        let mut sim = build()
            .diagnostic_policy(policy("interface_array_query", level))
            .build_cranelift()
            .unwrap();
        assert_eq!(sim.get(sim.signal("y")), 2u8.into());
        assert_eq!(
            sim.warnings().len(),
            usize::from(level == DiagnosticLevel::Warn)
        );
        if let Some(warning) = sim.warnings().first() {
            let rendered = celox::render_diagnostic(warning);
            assert!(rendered.contains("size_extension.sv"), "{rendered}");
        }
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn interface_array_extension_keeps_interfaces_out_of_data_expressions() {
    use std::path::Path;
    for expression in [
        "$bits(lanes)",
        "$left(lanes)",
        "$size(lanes[0])",
        "$size(lanes,missing)",
        "$size(lanes,dim)",
        "lanes",
    ] {
        let source = format!(
            "interface Lane; logic v; endinterface module Top(input int dim,output wire [31:0] y); Lane lanes[2](); assign y={expression}; endmodule"
        );
        assert!(
            Simulator::from_sv_sources(vec![(&source, Path::new("bad_interface_query.sv"))], "Top")
                .diagnostic_policy(policy("interface_array_query", DiagnosticLevel::Allow))
                .build_cranelift()
                .is_err(),
            "{expression}"
        );
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn interface_array_query_warning_survives_a_later_analysis_error() {
    use std::path::Path;
    let source = "interface Lane; logic v; endinterface module Top(output wire y); Lane lanes[2](); localparam N=$size(lanes); assign y=missing; endmodule";
    let error = Simulator::from_sv_sources(vec![(source, Path::new("later_error.sv"))], "Top")
        .diagnostic_policy(policy("interface_array_query", DiagnosticLevel::Warn))
        .build_cranelift()
        .err()
        .unwrap();
    assert!(error.to_string().contains("missing"), "{error}");
    assert_eq!(error.warnings().len(), 1, "{error}");
}

#[cfg(feature = "systemverilog")]
#[test]
fn interface_array_query_policy_covers_formals_and_generated_scopes() {
    use std::path::Path;
    for source in [
        "interface Lane; logic v; endinterface module Reader(Lane ports[2],output wire [7:0] y); assign y=$size(ports); endmodule module Top(output wire [7:0] y); Lane lanes[2](); Reader r(.ports(lanes),.y(y)); endmodule",
        "interface Lane; logic v; endinterface module Top(output wire [7:0] y); if(1) begin: bank Lane lanes[2](); assign y=$size(lanes); end endmodule",
        "interface Lane; logic v; endinterface module Top(output wire [7:0] y); Lane lanes[2](); function automatic int count(); return $size(lanes); endfunction assign y=count(); endmodule",
    ] {
        let error = Simulator::from_sv_sources(vec![(source, Path::new("query_scope.sv"))], "Top")
            .build_cranelift()
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("interface_array_query"),
            "{error}"
        );
        let mut sim =
            Simulator::from_sv_sources(vec![(source, Path::new("query_scope.sv"))], "Top")
                .diagnostic_policy(policy("interface_array_query", DiagnosticLevel::Allow))
                .build_cranelift()
                .unwrap();
        assert_eq!(sim.get(sim.signal("y")), 2u8.into());
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn invalid_interface_array_query_dimensions_return_integer_unknowns() {
    use std::path::Path;
    let source = "interface Lane; logic v; endinterface module Top(output wire [31:0] zero,extra,output wire [63:0] wide); Lane lanes[2](); assign zero=$size(lanes,0); assign extra=$size(lanes,2); assign wide=$size(lanes,2); endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("query_rank.sv"))], "Top")
        .four_state(true)
        .diagnostic_policy(policy("interface_array_query", DiagnosticLevel::Allow))
        .build_cranelift()
        .unwrap();
    let wide = sim.signal("wide");
    assert_eq!(sim.get_four_state(wide), (u64::MAX.into(), u64::MAX.into()));
    for name in ["zero", "extra"] {
        let signal = sim.signal(name);
        assert_eq!(
            sim.get_four_state(signal),
            (u32::MAX.into(), u32::MAX.into())
        );
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn unindexed_interface_array_parameters_have_a_located_recoverable_diagnostic() {
    use std::path::Path;
    let source = "interface Bus #(parameter WIDTH=8); logic [WIDTH-1:0] v; endinterface
        module Top(output wire [31:0] q); Bus #(13) bus[5:4](); assign q=bus.WIDTH; endmodule";
    let build =
        || Simulator::from_sv_sources(vec![(source, Path::new("array_parameter.sv"))], "Top");
    let error = build().build_cranelift().err().unwrap();
    assert!(
        matches!(error.kind(), SimulatorErrorKind::DiagnosticPolicy(_)),
        "{error}"
    );
    assert!(
        error.to_string().contains("interface_array_parameter"),
        "{error}"
    );
    for level in [DiagnosticLevel::Warn, DiagnosticLevel::Allow] {
        let mut sim = build()
            .diagnostic_policy(policy("interface_array_parameter", level))
            .build_cranelift()
            .unwrap();
        assert_eq!(sim.get(sim.signal("q")), 13u8.into());
        assert_eq!(
            sim.warnings().len(),
            usize::from(level == DiagnosticLevel::Warn)
        );
        if let Some(warning) = sim.warnings().first() {
            let rendered = celox::render_diagnostic(warning);
            assert!(rendered.contains("array_parameter.sv"), "{rendered}");
            assert!(rendered.contains("bus[5]"), "{rendered}");
        }
    }
}

#[cfg(feature = "systemverilog")]
#[test]
fn unindexed_interface_parameter_lookup_respects_lexical_shadowing() {
    use std::path::Path;
    let source = "interface Bus #(parameter WIDTH=13); endinterface
        module Top(output wire [31:0] q);
        Bus bus[2]();
        typedef struct packed {logic [7:0] WIDTH;} Word;
        function automatic int f(input Word bus); return bus.WIDTH; endfunction
        localparam VALUE=f(8'd42); assign q=VALUE; endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("shadowed_bus.sv"))], "Top")
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("q")), 42u8.into());
    assert!(sim.warnings().is_empty());
}

#[cfg(feature = "systemverilog")]
#[test]
fn unindexed_interface_parameters_resolve_inside_indexed_generate_paths() {
    use std::path::Path;
    let source = "interface Bus #(parameter WIDTH=8); endinterface
        module Top(output wire [31:0] q);
        for(genvar i=0;i<2;i=i+1) begin:g Bus #(10+i) bus[5:4](); end
        assign q=g[1].bus.WIDTH; endmodule";
    let mut sim = Simulator::from_sv_sources(vec![(source, Path::new("generated_bus.sv"))], "Top")
        .diagnostic_policy(policy("interface_array_parameter", DiagnosticLevel::Warn))
        .build_cranelift()
        .unwrap();
    assert_eq!(sim.get(sim.signal("q")), 11u8.into());
    assert_eq!(sim.warnings().len(), 1);
}
