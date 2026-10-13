//! What a suite supplies to build its designs with an external simulator.

mod timed_ff;

use crate::process::write_if_changed;
use crate::script::ScriptCase;
use crate::script::sv::{DesignInfo, TESTBENCH_TOP, testbench as generate_testbench};
use crate::script::timed::{after_trivia, sv_words};
use crate::validate_timed_design;
use crate::verification::{EmissionError, panic_message};
use crate::{Design, Expectation, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The module that instantiates the top with a rejection case's parameters.
const PARAMETERS_TOP: &str = "celox_suite_parameters";

/// The time scale every compiled source gets: a script time unit is 1 ns,
/// and the generated timed testbench settles without advancing time. The
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
/// case that simulates, the generated testbench, into `directory`.
pub(crate) fn stage(
    frontend: &dyn Frontend,
    design: &Design,
    directory: &Path,
    script: Option<&ScriptCase>,
    defines: &[&str],
) -> Result<Staged> {
    validate_timed_design(design)?;
    fs::create_dir_all(directory)?;
    let directory = fs::canonicalize(directory)?;
    for (index, source) in design.sources.iter().enumerate() {
        write_if_changed(
            &directory.join(format!("input_{index}.{}", frontend.source_extension())),
            source.text.as_bytes(),
        )?;
    }
    let mut prepared =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frontend.prepare(design)))
            .map_err(|error| EmissionError(panic_message(error.as_ref())))??;
    if design.timed {
        for source in &mut prepared.sources {
            *source = expanded_timed_source(source, defines)?;
        }
    }
    let task_timing = timed_ff::TaskTiming::new(&prepared.sources);
    let control_names = timed_names(&prepared.sources);
    let timescale = directory.join("timescale.sv");
    write_if_changed(&timescale, TIMESCALE.as_bytes())?;
    let mut paths = vec![timescale];
    if design.timed {
        let control = directory.join("timed_control.sv");
        write_if_changed(&control, control_names.generated(TIMED_CONTROL).as_bytes())?;
        paths.push(control);
    }
    for (index, source) in prepared.sources.iter().enumerate() {
        let path = directory.join(format!("source_{index}.sv"));
        let source = if design.timed {
            timed_source(source, &control_names, &task_timing)?
        } else {
            source.clone()
        };
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
        let info = prepared
            .info
            .as_ref()
            .ok_or("the frontend gives no port information for a generated testbench")?;
        let path = directory.join("testbench.sv");
        let bench = generate_testbench(case, info)?;
        let bench = if design.timed {
            control_names.generated(&bench)
        } else {
            bench
        };
        write_if_changed(&path, bench.as_bytes())?;
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

const TIMED_CONTROL: &str = r#"package celox_suite_timed_control;
  bit started = 0;
  bit finished = 0;
  function automatic void celox_suite_start_design();
    started = 1;
  endfunction
  function automatic void celox_suite_finish_design(input int code = 1);
    finished = 1;
    case (code)
      0: $finish(0);
      2: $finish(2);
      default: $finish(1);
    endcase
  endfunction
endpackage
"#;

struct TimedNames {
    package: String,
    start: String,
    finish: String,
}

fn timed_names(sources: &[String]) -> TimedNames {
    let words = sources
        .iter()
        .flat_map(|source| sv_words(source).into_iter().map(|(_, _, word)| word))
        .collect::<std::collections::BTreeSet<_>>();
    for suffix in 0u64.. {
        let name = |base: &str| {
            if suffix == 0 {
                base.to_string()
            } else {
                format!("{base}_{suffix}")
            }
        };
        let names = TimedNames {
            package: name("celox_suite_timed_control"),
            start: name("celox_suite_start_design"),
            finish: name("celox_suite_finish_design"),
        };
        if [&names.package, &names.start, &names.finish]
            .into_iter()
            .all(|name| !words.contains(name.as_str()))
        {
            return names;
        }
    }
    unreachable!()
}
impl TimedNames {
    fn generated(&self, source: &str) -> String {
        let mut output = String::new();
        let mut previous = 0;
        for (start, end, word) in sv_words(source) {
            // Generated package references and unqualified helper calls; leave
            // source signal paths (dut.name) and quoted script text intact.
            if source[..start].trim_end().ends_with('.') {
                continue;
            }
            let prefix = source[..start].trim_end();
            let tail = after_trivia(&source[end..]);
            let previous_word = sv_words(prefix).last().map(|word| word.2);
            let replacement = match word {
                "celox_suite_timed_control"
                    if tail.starts_with("::")
                        || matches!(previous_word, Some("package" | "import")) =>
                {
                    &self.package
                }
                "celox_suite_start_design" if tail.starts_with('(') => &self.start,
                "celox_suite_finish_design" if tail.starts_with('(') => &self.finish,
                _ => continue,
            };
            output.push_str(&source[previous..start]);
            output.push_str(replacement);
            previous = end;
        }
        output.push_str(&source[previous..]);
        output
    }
}

fn expanded_timed_source(source: &str, defines: &[&str]) -> Result<String> {
    let prefix = defines
        .iter()
        .map(|name| format!("`define {name}\n"))
        .collect::<String>();
    let syntax = celox_sv_analyzer::syntax::parse_source(
        &format!("{prefix}`line 1 \"timed.sv\" 0\n{source}"),
        Path::new("timed.sv"),
    )?;
    let root = (&syntax).into_iter().next().ok_or("empty timed source")?;
    Ok(syntax
        .get_str(vec![root])
        .ok_or("missing timed source text")?
        .to_owned())
}

fn timed_source(source: &str, names: &TimedNames, tasks: &timed_ff::TaskTiming) -> Result<String> {
    Ok(format!(
        "import {}::{};\n{}",
        names.package,
        names.finish,
        timed_content(source, names, tasks)?
    ))
}

fn timed_content(source: &str, names: &TimedNames, tasks: &timed_ff::TaskTiming) -> Result<String> {
    let mut output = String::new();
    let mut previous = 0;
    for (start, end, word) in sv_words(source) {
        if start < previous {
            continue;
        }
        if word == "always_ff" || (word == "always" && timed_ff::edge_control(source, end, tasks)) {
            let (body_end, replacement) = timed_ff::gated(source, start, end, names, tasks)?;
            output.push_str(&source[previous..start]);
            output.push_str(&timed_ff::in_macro(source, start, replacement));
            previous = body_end;
            continue;
        }
        let replacement = match word {
            "initial" => format!("initial wait ({}::started)", names.package),
            "always" if timed_ff::wildcard_control(source, end) => continue,
            "always" => {
                format!("always wait ({}::started)", names.package)
            }
            "$finish" if after_trivia(&source[end..]).starts_with('(') => names.finish.clone(),
            "$finish" => format!("{}()", names.finish),
            _ => continue,
        };
        output.push_str(&source[previous..start]);
        output.push_str(&replacement);
        previous = end;
    }
    output.push_str(&source[previous..]);
    Ok(output)
}

#[cfg(all(test, feature = "icarus", feature = "verilator"))]
mod timed_staging_tests {
    use crate as celox_test_suite;
    use crate::{Backend, load_group, no_std_library};

    #[test]
    #[ignore = "requires Icarus, Verilator and a C++ compiler; Celox does not yet support hierarchical task calls"]
    fn qualified_timed_tasks_do_not_use_same_named_local_tasks() {
        let text = r##"(group qualified (category sequential))
        (case task (source "task.sv" #"module Worker(output bit q);
            task tick; #1 q=1; endtask
          endmodule
          module Top(input bit clk, output bit q);
            Worker worker(.q(q)); bit dummy;
            task tick; dummy=1; endtask
            always @(posedge clk) worker.tick();
          endmodule"#)
          (top Top) (set clk 1) (run_until 1) (assert_eq q 0)
          (set clk 0) (eval) (set clk 1) (run_until 2) (assert_eq q 1))"##;
        let case = load_group("qualified-task.vtest", text, no_std_library)[0];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/qualified-task-protocol")
            .join(std::process::id().to_string());
        struct TaskFrontend;
        impl celox_test_suite::frontend::Frontend for TaskFrontend {
            fn source_extension(&self) -> &'static str {
                "sv"
            }
            fn prepare(
                &self,
                design: &celox_test_suite::Design,
            ) -> celox_test_suite::Result<celox_test_suite::frontend::PreparedDesign> {
                let frontend = celox_test_suite::sv::verification::SvFrontend;
                let mut actual = frontend.prepare(design)?;
                // Port shapes are known independently of unsupported method-call
                // analysis; this test exercises the external staging adapter.
                let stub = celox_test_suite::Design::new(
                    "module Top(input bit clk, output bit q); endmodule",
                    "Top",
                );
                actual.info = frontend.prepare(&stub)?.info;
                Ok(actual)
            }
        }
        let frontend = TaskFrontend;
        let mut icarus =
            celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
                .unwrap();
        icarus.run_testbench().unwrap();
        let mut verilator = celox_test_suite::verilator::Verilator::build_script(
            &frontend,
            case,
            &root.join("verilator"),
        )
        .unwrap();
        verilator.run_testbench().unwrap();
        let log = std::fs::read_to_string(root.join("verilator/protocol.log")).unwrap();
        celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires Icarus, Verilator and a C++ compiler; frontend lacks implicit generate hierarchy analysis"]
    fn ff_staging_preserves_implicit_generate_names() {
        let text = r##"(group names (category sequential))
(case names (source "names.sv" #"module Top(input bit clk,output bit q,y);
 always_ff @(posedge clk) q<=1;
 if(1) begin bit x;assign x=1;end
 assign y=genblk1.x;
 endmodule"#)(top Top)(set clk 1)(run_until 0)(assert_eq q 1)(assert_eq y 1))"##;
        let case = load_group("names.vtest", text, no_std_library)[0];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/generate-name-protocol")
            .join(std::process::id().to_string());
        struct NamesFrontend;
        impl celox_test_suite::frontend::Frontend for NamesFrontend {
            fn source_extension(&self) -> &'static str {
                "sv"
            }
            fn prepare(
                &self,
                design: &celox_test_suite::Design,
            ) -> celox_test_suite::Result<celox_test_suite::frontend::PreparedDesign> {
                let frontend = celox_test_suite::sv::verification::SvFrontend;
                let mut actual = frontend.prepare(design)?;
                // The adapter preserves names even when the frontend cannot
                // analyze the original implicit generate hierarchy.
                let stub = celox_test_suite::Design::new(
                    "module Top(input bit clk, output bit q,y); endmodule",
                    "Top",
                );
                actual.info = frontend.prepare(&stub)?.info;
                Ok(actual)
            }
        }
        let frontend = NamesFrontend;
        let mut icarus =
            celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
                .unwrap();
        icarus.run_testbench().unwrap();
        let mut verilator = celox_test_suite::verilator::Verilator::build_script(
            &frontend,
            case,
            &root.join("verilator"),
        )
        .unwrap();
        verilator.run_testbench().unwrap();
        let log = std::fs::read_to_string(root.join("verilator/protocol.log")).unwrap();
        celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires Icarus, Verilator and a C++ compiler; tests external compilation-unit startup staging"]
    fn compilation_unit_callables_retain_staged_edges() {
        let text = r##"(group unit (category sequential))
      (case callable (source "unit.sv" #"function bit tick();return 1;endfunction
        module Top(input bit clk,output bit q,seen);always @(posedge clk)q<=tick();initial seen=q;endmodule"#)
        (top Top)(set clk 1)(run_until 0)(assert_eq seen 0)(assert_eq q 1))"##;
        let case = load_group("unit.vtest", text, no_std_library)[0];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/unit-callable-protocol")
            .join(std::process::id().to_string());
        let frontend = celox_test_suite::sv::verification::SvFrontend;
        let mut icarus =
            celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
                .unwrap();
        icarus.run_testbench().unwrap();
        let mut verilator = celox_test_suite::verilator::Verilator::build_script(
            &frontend,
            case,
            &root.join("verilator"),
        )
        .unwrap();
        verilator.run_testbench().unwrap();
        let log = std::fs::read_to_string(root.join("verilator/protocol.log")).unwrap();
        celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires Icarus, Verilator and a C++ compiler; exercises external declaration staging"]
    fn local_declarations_retain_staged_edges() {
        let text = r##"(group declarations (category sequential))
        (case local (source "decl.sv" #"module Top(input bit clk,output bit q);
          task tick; #1; endtask
          always @(posedge clk)begin int tick; q<=1; end endmodule"#)
          (top Top)(set clk 1)(run_until 0)(assert_eq q 1))"##;
        let case = load_group("decl.vtest", text, no_std_library)[0];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/declaration-staging")
            .join(std::process::id().to_string());
        let frontend = celox_test_suite::sv::verification::SvFrontend;
        let mut icarus =
            celox_test_suite::icarus::Icarus::build_script(&frontend, case, &root.join("icarus"))
                .unwrap();
        icarus.run_testbench().unwrap();
        let mut verilator = celox_test_suite::verilator::Verilator::build_script(
            &frontend,
            case,
            &root.join("verilator"),
        )
        .unwrap();
        verilator.run_testbench().unwrap();
        let log = std::fs::read_to_string(root.join("verilator/protocol.log")).unwrap();
        celox_test_suite::script::sv::check_output(&log, &[]).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    #[ignore = "requires Verilator and a C++ compiler; Icarus rejects deferred assertions"]
    fn deferred_assertions_retain_staged_edges() {
        let text = r##"(group deferred (category sequential))
          (case assertion (source "assert.sv" #"module Top(input bit clk,output bit q,seen);
            task accept; q<=1; endtask
            task reject; q<=0; endtask
            always @(posedge clk) assert #0 (1) accept(); else reject();
            initial seen=q;
          endmodule"#) (top Top) (set clk 1) (run_until 0) (assert_eq seen 0) (assert_eq q 1))"##;
        let case = load_group("deferred.vtest", text, no_std_library)[0];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/deferred-assertion-protocol")
            .join(std::process::id().to_string());
        let mut backend = celox_test_suite::verilator::Verilator::build_script(
            &celox_test_suite::sv::verification::SvFrontend,
            case,
            &root,
        )
        .unwrap();
        backend.run_testbench().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod adapter_define_tests {
    use super::*;
    #[test]
    fn timed_macro_expansion_uses_the_adapter_define_set() {
        let source = "module Top;\n`ifdef CELOX_SUITE_ICARUS\ninitial q=1;\n`else\ninitial q=0;\n`endif\nendmodule";
        let icarus = expanded_timed_source(source, &["CELOX_SUITE_ICARUS"]).unwrap();
        let other = expanded_timed_source(source, &[]).unwrap();
        assert!(icarus.contains("q=1"));
        assert!(!icarus.contains("q=0"));
        assert!(other.contains("q=0"));
        assert!(!other.contains("q=1"));
    }
}
