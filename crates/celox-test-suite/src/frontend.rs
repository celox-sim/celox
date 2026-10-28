//! What a suite supplies to build its designs with an external simulator.

use crate::process::write_if_changed;
use crate::script::ScriptCase;
use crate::script::sv::{DesignInfo, TESTBENCH_TOP, testbench as generate_testbench};
use crate::script::timed::validate_timed_design_with_defines;
use crate::verification::{EmissionError, panic_message};
use crate::{Design, Expectation, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The module that instantiates the top with a rejection case's parameters.
const PARAMETERS_TOP: &str = "celox_suite_parameters";

/// The time scale a timed design is compiled with: a script time unit is
/// 1 ns, and the generated testbench settles without advancing time. The
/// directive persists across the files that follow it in one compilation.
const TIMESCALE: &str = "`timescale 1ns/1ps\n";

/// A design translated to SystemVerilog for an external simulator.
pub struct PreparedDesign {
    /// SystemVerilog sources to compile, in order.
    pub sources: Vec<String>,
    /// The module to elaborate: the design's top, or its native testbench.
    pub top: String,
    /// Whether `top` is a self-checking native testbench.
    pub native_testbench: bool,
    /// Clock and reset ports of the top module; `true` is a rising edge.
    pub edges: BTreeMap<String, bool>,
    /// Port shapes for a generated testbench, when they are known.
    pub info: Option<DesignInfo>,
}

/// Turns a suite's designs into SystemVerilog. A panic while preparing is
/// reported as an emission error, never as a simulator result.
pub trait Frontend: Sync {
    /// The extension of the original source files kept beside each build.
    fn source_extension(&self) -> &'static str;

    fn prepare(&self, design: &Design) -> Result<PreparedDesign>;
}

/// The files and settings an adapter compiles.
pub(crate) struct Staged {
    pub directory: PathBuf,
    pub paths: Vec<PathBuf>,
    pub top: String,
    pub testbench: bool,
    pub edges: BTreeMap<String, bool>,
}

/// Write the original and translated sources of `design`, and for a script
/// case that simulates, the generated testbench, into `directory`. `defines`
/// are the macros the adapter's compiler predefines.
pub(crate) fn stage(
    frontend: &dyn Frontend,
    design: &Design,
    directory: &Path,
    script: Option<&ScriptCase>,
    defines: &[&str],
) -> Result<Staged> {
    validate_timed_design_with_defines(design, defines)?;
    fs::create_dir_all(directory)?;
    let directory = fs::canonicalize(directory)?;
    for (index, source) in design.sources.iter().enumerate() {
        write_if_changed(
            &directory.join(format!("input_{index}.{}", frontend.source_extension())),
            source.text.as_bytes(),
        )?;
    }
    let prepared =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frontend.prepare(design)))
            .map_err(|error| EmissionError(panic_message(error.as_ref())))??;
    let mut paths = Vec::new();
    if design.timed {
        let timescale = directory.join("timescale.sv");
        write_if_changed(&timescale, TIMESCALE.as_bytes())?;
        paths.push(timescale);
    }
    for (index, source) in prepared.sources.iter().enumerate() {
        let path = directory.join(format!("source_{index}.sv"));
        write_if_changed(&path, source.as_bytes())?;
        paths.push(path);
    }
    let mut top = prepared.top;
    let mut testbench = prepared.native_testbench;
    // A rejection needs only the design, instantiated with the case's
    // parameter values when it has any.
    if !design.parameters.is_empty()
        && !testbench
        && script.is_none_or(|case| case.expectation == Expectation::CompilationError)
    {
        let overrides = design
            .parameters
            .iter()
            .map(|(name, value)| format!(".{name}({value})"))
            .collect::<Vec<_>>()
            .join(", ");
        let path = directory.join("parameters.sv");
        // The wrapper leaves the ports open; that is not part of the design.
        let wrapper = format!(
            "module {PARAMETERS_TOP};\n  /* verilator lint_off PINMISSING */\n  \
             {top} #({overrides}) dut ();\nendmodule\n"
        );
        write_if_changed(&path, wrapper.as_bytes())?;
        paths.push(path);
        top = PARAMETERS_TOP.to_string();
    }
    if let Some(case) = script.filter(|case| case.expectation != Expectation::CompilationError) {
        crate::script::sv::validate_final_signal_reads(case, &prepared.sources)?;
        let info = prepared
            .info
            .as_ref()
            .ok_or("the frontend gives no port information for a generated testbench")?;
        let path = directory.join("testbench.sv");
        write_if_changed(&path, generate_testbench(case, info)?.as_bytes())?;
        paths.push(path);
        top = TESTBENCH_TOP.to_string();
        testbench = true;
    }
    Ok(Staged {
        directory,
        paths,
        top,
        testbench,
        edges: prepared.edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Emitted(Vec<String>);
    impl Frontend for Emitted {
        fn source_extension(&self) -> &'static str {
            "sv"
        }
        fn prepare(&self, _: &Design) -> Result<PreparedDesign> {
            Ok(PreparedDesign {
                sources: self.0.clone(),
                top: "Top".into(),
                native_testbench: false,
                edges: Default::default(),
                info: Some(DesignInfo {
                    top: "Top".into(),
                    inputs: vec![],
                    outputs: vec!["value".into()],
                    edges: Default::default(),
                    max_width: 32,
                    parameters: vec![],
                    arrays: Default::default(),
                }),
            })
        }
    }

    fn staged(sources: Vec<String>, script: &str) -> Result<Staged> {
        // The original source has no final: validation must inspect emitted SV.
        let text = format!(
            r##"(group timing (category sequential))
            (case finals (source "top.sv" #"module Top; endmodule"#) (top Top) {script})"##
        );
        let case = crate::load_group("timing.vtest", &text, crate::no_std_library)[0];
        let directory = tempfile::tempdir()?;
        stage(
            &Emitted(sources),
            &case.design(),
            directory.path(),
            Some(case.script()),
            &[],
        )
    }

    #[test]
    fn post_finish_signal_reads_reject_potential_final_effects() {
        for body in [
            "value=1;",
            "begin value=1; end",
            "write_value();",
            "$display(\"%0d\", write_value());",
            "$display(\"%0d\", write_value);",
            "$display(\"%0d\", value++);",
            "$display(\"%0d\", (value=1));",
            "$display(\"%0d\", $urandom(value));",
        ] {
            let source = format!(
                "module Top(output int value); \
                 function int write_value(); value=1; return 1; endfunction \
                 final {body} endmodule"
            );
            for check in [
                "(assert_eq value 1)",
                "(assert_eq (+ (get value) 0) 1)",
                "(assert_eq (+ 1 0) (get value))",
                "(assert (== (get value) 1))",
            ] {
                let error = staged(vec![source.clone()], &format!("(run_to_finish) {check}"))
                    .err()
                    .expect("potential final writes must not race a signal check");
                assert!(
                    error.to_string().contains("final ordering is unspecified"),
                    "{error}"
                );
            }
            staged(vec![source], "(run_to_finish) (expect_output \"done\")").unwrap();
        }
        let sources = vec![
            "`define CLEANUP final begin value=1; end\n".into(),
            "module Top(output int value); `CLEANUP endmodule".into(),
        ];
        assert!(staged(sources, "(run_to_finish) (assert_eq value 1)").is_err());
        // Writes in another module can affect an output through hierarchy or
        // combinational dependencies; do not try matching assignment names.
        assert!(
            staged(
                vec![
                    "module Top(output int value); Child c(value); endmodule\n\
             module Child(output int v); final v=1; endmodule"
                        .into()
                ],
                "(run_to_finish) (assert_eq value 1)"
            )
            .is_err()
        );
        let escaped = r#"module Top(output int value);
            function void \$display (input string text); value=1; endfunction
            final \$display ("done"); endmodule"#;
        assert!(staged(vec![escaped.into()], "(run_to_finish) (assert_eq value 1)").is_err());
    }

    #[test]
    fn post_finish_checks_keep_safe_final_output_and_captured_values() {
        for source in [
            "module Top(output int value); endmodule",
            "module Top(output int value); final $write(\"final\"); endmodule",
            "module Top(output int value); final begin $display(\"at %0d\", $time); $write(\"done\"); end endmodule",
            "module Top(output int value); final $display(\"%0d %0f\", $time(), $realtime()); endmodule",
            "module Top(output int value); initial assert final (1); endmodule",
            "module Top(output int value);\n`ifdef DISABLED\nfinal value=1;\n`endif\nendmodule",
        ] {
            staged(vec![source.into()], "(run_to_finish) (assert_eq value 0)")
                .unwrap_or_else(|error| panic!("{source}: {error}"));
        }
        let source = "module Top(output int value); final value=1; endmodule";
        for script in [
            "(run_to_finish) (assert (== 1 1))",
            "(run_until 0) (let saved (get value)) (run_to_finish) (assert_eq (+ saved 0) 0)",
            "(run_until 0) (assert_eq value 0) (run_to_finish) (expect_output \"\")",
        ] {
            staged(vec![source.into()], script).unwrap();
        }
    }

    #[test]
    fn shared_sv_post_finish_checks_are_independent_of_final_order() {
        for case in
            crate::sv::cases().filter(|case| case.expectation == crate::Expectation::Simulation)
        {
            let sources = case
                .design()
                .sources
                .into_iter()
                .map(|source| source.text)
                .collect::<Vec<_>>();
            crate::script::sv::validate_final_signal_reads(case.script(), &sources)
                .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        }
    }
}
