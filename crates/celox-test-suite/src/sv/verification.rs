//! Verification of this suite against the external simulators.

use crate::script::sv::{Bounds, DesignInfo, Port};
use crate::verification::{Suite, Tool};
use crate::{Design, Frontend, PreparedDesign, Result};
use celox_sv_analyzer::ir::{FfEdge, Module, PortDirection, Type, UnpackedRange};
use celox_sv_analyzer::typecheck::eval_const_expr;
use std::collections::BTreeMap;

/// Builds SystemVerilog designs for the external simulators: the sources are
/// compiled as written. Port shapes and clock polarity for a generated
/// testbench come from the Celox SystemVerilog analyzer, which only reads
/// declarations here; a design it cannot analyze gets no generated testbench.
pub struct SvFrontend;

fn bounds(range: &UnpackedRange) -> Option<Bounds> {
    let constants = Default::default();
    Some(Bounds {
        left: i64::try_from(eval_const_expr(range.left(), &constants)?).ok()?,
        right: i64::try_from(eval_const_expr(range.right(), &constants)?).ok()?,
    })
}

/// The element width and bounds of a one-dimensional unpacked array type.
fn array(r#type: &Type) -> Option<(usize, Bounds)> {
    let [range] = r#type.unpacked_ranges() else {
        return None;
    };
    let bounds = bounds(range)?;
    Some((r#type.resolved_width()? / bounds.count(), bounds))
}

fn element_width(r#type: &Type) -> Option<usize> {
    match array(r#type) {
        Some((width, _)) => Some(width),
        None if r#type.unpacked_ranges().is_empty() => r#type.resolved_width(),
        None => None,
    }
}

/// The preprocessed `sources` with every procedure and task blanked out, for
/// a timed design's declarations: the analyzer does not lower every timing
/// control, and port shapes do not depend on process bodies. Blanking keeps
/// line breaks, so declaration positions are unchanged.
fn declarations_only(design: &Design) -> Option<Design> {
    let mut defines = sv_parser::Defines::default();
    let mut declarations = design.clone();
    for source in &mut declarations.sources {
        let includes: Vec<&std::path::Path> = Vec::new();
        let (text, next) = sv_parser::preprocess_str(
            &source.text,
            &source.path,
            &defines,
            &includes,
            true,
            false,
            0,
            0,
        )
        .ok()?;
        let mut blanked = text.text().as_bytes().to_vec();
        let (tree, _) = sv_parser::parse_sv_pp(text, next.clone(), false).ok()?;
        defines = next;
        for node in &tree {
            if !matches!(
                node,
                sv_parser::RefNode::InitialConstruct(_)
                    | sv_parser::RefNode::AlwaysConstruct(_)
                    | sv_parser::RefNode::FinalConstruct(_)
                    | sv_parser::RefNode::TaskDeclaration(_)
            ) {
                continue;
            }
            let locates = node.into_iter().filter_map(|inner| match inner {
                sv_parser::RefNode::Locate(locate) => Some(*locate),
                _ => None,
            });
            for locate in locates {
                for byte in &mut blanked[locate.offset..locate.offset + locate.len] {
                    if *byte != b'\n' {
                        *byte = b' ';
                    }
                }
            }
        }
        source.text = String::from_utf8(blanked).ok()?;
    }
    Some(declarations)
}

fn design_info(design: &Design) -> Option<DesignInfo> {
    if design.timed {
        return analyzed_info(&declarations_only(design)?);
    }
    analyzed_info(design)
}

fn analyzed_info(design: &Design) -> Option<DesignInfo> {
    // Interfaces are expanded as the Celox frontend does; the tools still
    // compile the sources as written.
    let paths: Vec<_> = design
        .sources
        .iter()
        .map(|source| (source.text.as_str(), source.path.as_path()))
        .collect();
    let texts = celox_sv_analyzer::elaborate_interfaces(&paths)
        .ok()?
        .unwrap_or_else(|| {
            design
                .sources
                .iter()
                .map(|source| source.text.clone())
                .collect()
        });
    let sources: Vec<_> = texts
        .iter()
        .zip(&design.sources)
        .map(|(text, source)| (text.as_str(), source.path.as_path()))
        .collect();
    let packages = celox_sv_analyzer::analyze_packages(&sources).ok()?;
    // The top takes the case's parameter values. A module that cannot be
    // analyzed on its own, such as one with a generic interface port that the
    // elaboration copies per binding, does not hide the others.
    let mut modules: Vec<Module> = Vec::new();
    for &(text, path) in &sources {
        for name in celox_sv_analyzer::source_module_names(text, path).unwrap_or_default() {
            let overrides = if name == design.top {
                design
                    .parameters
                    .iter()
                    .map(|(name, value)| (name.clone(), i128::from(*value)))
                    .collect()
            } else {
                Default::default()
            };
            if let Ok(ir) = celox_sv_analyzer::analyze_source_module_with_packages(
                text, path, &name, &overrides, &packages,
            ) {
                modules.extend(ir.modules().iter().cloned());
            }
        }
    }
    let top = modules.iter().find(|module| module.name() == design.top)?;
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut arrays = BTreeMap::new();
    for port in top.ports() {
        match port.direction() {
            PortDirection::Input => inputs.push(Port {
                name: port.name().to_string(),
                width: element_width(port.r#type())?,
                array: array(port.r#type()).map(|(_, bounds)| bounds),
            }),
            PortDirection::Output => outputs.push(port.name().to_string()),
            _ => return None,
        }
        if let Some(shape) = array(port.r#type()) {
            arrays.insert(port.name().to_string(), shape);
        }
    }
    for signal in top.signals() {
        if let Some(shape) = array(signal.r#type()) {
            arrays.insert(signal.name().to_string(), shape);
        }
    }
    // A port is ticked on the edge its flip-flops wait for; rising by default.
    let mut edges = BTreeMap::new();
    for port in &inputs {
        let mut falling = false;
        let mut rising = false;
        for module in &modules {
            for process in module.ff_processes() {
                for event in process.events() {
                    if event.signal() == port.name {
                        match event.edge() {
                            FfEdge::Pos => rising = true,
                            FfEdge::Neg => falling = true,
                        }
                    }
                }
            }
        }
        if port.width == 1 && port.array.is_none() {
            edges.insert(port.name.clone(), rising || !falling);
        }
    }
    let max_width = modules
        .iter()
        .flat_map(|module| {
            module
                .ports()
                .iter()
                .map(|port| port.r#type())
                .chain(module.signals().iter().map(|signal| signal.r#type()))
        })
        .filter_map(element_width)
        .max()
        .unwrap_or(1);
    Some(DesignInfo {
        top: design.top.clone(),
        inputs,
        outputs,
        edges,
        max_width,
        parameters: design.parameters.clone(),
        arrays,
    })
}

impl Frontend for SvFrontend {
    fn source_extension(&self) -> &'static str {
        "sv"
    }

    fn prepare(&self, design: &Design) -> Result<PreparedDesign> {
        let info = design_info(design);
        Ok(PreparedDesign {
            sources: design
                .sources
                .iter()
                .map(|source| source.text.clone())
                .collect(),
            top: design.top.clone(),
            native_testbench: false,
            edges: info
                .as_ref()
                .map(|info| info.edges.clone())
                .unwrap_or_default(),
            info,
        })
    }
}

/// The reviewed exclusion of `case` for `tool`, if any.
pub fn known_issue(tool: &str, case: &str) -> Option<serde_json::Value> {
    static LIMITATIONS: std::sync::OnceLock<Vec<serde_json::Value>> = std::sync::OnceLock::new();
    let limitations = LIMITATIONS.get_or_init(|| {
        serde_json::from_str(include_str!("../../verification/sv/limitations.json"))
            .expect("checked-in verification limitations must be valid JSON")
    });
    limitations.iter().find_map(|group| {
        let cases = group["cases"][tool].as_array()?;
        cases
            .iter()
            .any(|name| name.as_str() == Some(case))
            .then(|| {
                let mut metadata = group.clone();
                metadata.as_object_mut().unwrap().remove("cases");
                metadata
            })
    })
}

/// This suite, for the shared runner.
pub fn suite() -> Suite {
    Suite {
        name: "sv",
        cases: crate::sv::cases().collect(),
        known_issue,
    }
}

/// The command-line runner of `verify-sv-verilator` and `verify-sv-icarus`.
pub fn run(tool: Tool) -> Result<()> {
    crate::verification::run(tool, &suite(), &SvFrontend)
}
