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
    let prepared =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frontend.prepare(design)))
            .map_err(|error| EmissionError(panic_message(error.as_ref())))??;
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
            timed_source(source, &control_names)?
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
            let replacement = match word {
                "celox_suite_timed_control" => &self.package,
                "celox_suite_start_design" => &self.start,
                "celox_suite_finish_design" => &self.finish,
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

fn timed_source(source: &str, names: &TimedNames) -> Result<String> {
    // Stage actual declarations after macro expansion, including process
    // keywords supplied as macro arguments.
    let syntax = celox_sv_analyzer::syntax::parse_source(source, Path::new("timed.sv"))?;
    let root = (&syntax).into_iter().next().ok_or("empty timed source")?;
    let expanded = syntax
        .get_str(vec![root])
        .ok_or("missing timed source text")?;
    Ok(format!(
        "import {}::*;\n{}",
        names.package,
        timed_content(expanded, names)?
    ))
}

fn timed_content(source: &str, names: &TimedNames) -> Result<String> {
    let mut output = String::new();
    let mut previous = 0;
    let timed_tasks = timed_ff::timed_tasks(source);
    for (start, end, word) in sv_words(source) {
        if start < previous {
            continue;
        }
        if word == "always_ff"
            || (word == "always" && timed_ff::edge_control(source, end, &timed_tasks))
        {
            let (body_end, replacement) = timed_ff::gated(source, start, end, names)?;
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
