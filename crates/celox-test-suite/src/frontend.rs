//! What a suite supplies to build its designs with an external simulator.

use crate::process::write_if_changed;
use crate::script::ScriptCase;
use crate::script::sv::{DesignInfo, TESTBENCH_TOP, testbench as generate_testbench};
use crate::verification::{EmissionError, panic_message};
use crate::{Design, Expectation, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
    for (index, source) in prepared.sources.iter().enumerate() {
        let path = directory.join(format!("source_{index}.sv"));
        write_if_changed(&path, source.as_bytes())?;
        paths.push(path);
    }
    let mut top = prepared.top;
    let mut testbench = prepared.native_testbench;
    // A rejection needs only the design.
    if let Some(case) = script.filter(|case| case.expectation != Expectation::CompilationError) {
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
