use crate::{HashMap, HashSet, flatting};
use thiserror::Error;

use crate::parser::module::ModuleParser;
use veryl_analyzer::ir::{Component, Module, VarId, VarKind, VarPath};
use veryl_analyzer::multi_sources::{MultiSources, Source};
use veryl_metadata::{ClockType, ResetType};
use veryl_parser::resource_table::{self, StrId};
use veryl_parser::token_range::TokenRange;

fn remap_for_fold_runtime_event_sites<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    start: usize,
    runtime_event_site_map: &HashMap<u32, u32>,
) -> Result<(), ParserError> {
    arena
        .remap_for_fold_effect_sites(start..arena.len(), |site_id, fatal_error_code| {
            Ok(runtime_event_site_map.get(&site_id).map(|&global_site| {
                (
                    global_site,
                    fatal_error_code.map(|_| i64::from(global_site)),
                )
            }))
        })
        .map_err(|error| {
            ParserError::illegal_context(
                "ForFold runtime-event relocation",
                error.to_string(),
                None,
            )
        })
}

fn verify_slt_roots<A>(
    arena: &SLTNodeArena<A>,
    paths: &[LogicPath<A>],
    observers: &[CombObserver<A>],
    variable_widths: &HashMap<A, usize>,
    variable_signedness: &HashMap<A, bool>,
    phase: &'static str,
) -> Result<(), ParserError>
where
    A: Hash + Eq + Clone,
{
    let facts =
        SLTNodeFacts::verify(arena).map_err(|error| ParserError::SltVerify { phase, error })?;
    let require = |node, role| {
        facts
            .require_lowerable(node, role)
            .map_err(|error| ParserError::SltVerify { phase, error })
    };
    let fail = |invariant, node, message| ParserError::SltVerify {
        phase,
        error: crate::logic_tree::SLTNodeFactsError::new(invariant, node, message),
    };
    let access_width = |access: crate::ir::BitAccess,
                        role: &'static str,
                        node: NodeId|
     -> Result<usize, ParserError> {
        let span = access.msb.checked_sub(access.lsb).ok_or_else(|| {
            fail(
                "ROOT.ACCESS_ORDERED",
                node,
                format!(
                    "{role} access has lsb {} greater than msb {}",
                    access.lsb, access.msb
                ),
            )
        })?;
        span.checked_add(1).ok_or_else(|| {
            fail(
                "ROOT.ACCESS_REPRESENTABLE",
                node,
                format!(
                    "{role} access [{}:{}] has an unrepresentable width",
                    access.msb, access.lsb
                ),
            )
        })
    };
    let verify_atom = |id: &A,
                       access: crate::ir::BitAccess,
                       role: &'static str,
                       node: NodeId|
     -> Result<usize, ParserError> {
        let width = access_width(access, role, node)?;
        let Some(&variable_width) = variable_widths.get(id) else {
            return Err(fail(
                "ROOT.VARIABLE_EXISTS",
                node,
                format!("{role} names a variable absent from the semantic type table"),
            ));
        };
        if variable_width == 0 || access.msb >= variable_width {
            return Err(fail(
                "ROOT.ACCESS_IN_VARIABLE_BOUNDS",
                node,
                format!(
                    "{role} access [{}:{}] is outside variable width {variable_width}",
                    access.msb, access.lsb
                ),
            ));
        }
        Ok(width)
    };

    for (node_index, node) in arena.iter().enumerate() {
        let node_id = NodeId(node_index);
        match node {
            crate::logic_tree::SLTNode::Input {
                variable, access, ..
            } => {
                verify_atom(variable, *access, "SLT input", node_id)?;
            }
            crate::logic_tree::SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                result,
                initials,
                updates,
                ..
            } => {
                let Some(&declared_loop_width) = variable_widths.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD.LOOP_VARIABLE_EXISTS",
                        node_id,
                        "ForFold loop variable is absent from the semantic type table".to_string(),
                    ));
                };
                if *loop_width != declared_loop_width {
                    return Err(fail(
                        "FOR_FOLD.LOOP_WIDTH_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFold loop width {loop_width} does not equal declared width {declared_loop_width}"
                        ),
                    ));
                }
                let Some(&declared_loop_signed) = variable_signedness.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD.LOOP_SIGNEDNESS_EXISTS",
                        node_id,
                        "ForFold loop variable signedness is absent from the semantic type table"
                            .to_string(),
                    ));
                };
                if *loop_signed != declared_loop_signed {
                    return Err(fail(
                        "FOR_FOLD.LOOP_SIGNEDNESS_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFold loop signedness {loop_signed} does not equal declared signedness {declared_loop_signed}"
                        ),
                    ));
                }
                verify_atom(&result.id, result.access, "ForFold result", node_id)?;
                for update in initials.iter().chain(updates) {
                    verify_atom(
                        &update.target.id,
                        update.target.access,
                        "ForFold state target",
                        node_id,
                    )?;
                }
            }
            crate::logic_tree::SLTNode::ForFoldGroup {
                loop_var,
                loop_width,
                loop_signed,
                states,
                ..
            } => {
                let Some(&declared_loop_width) = variable_widths.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_VARIABLE_EXISTS",
                        node_id,
                        "ForFoldGroup loop variable is absent from the semantic type table"
                            .to_string(),
                    ));
                };
                if *loop_width != declared_loop_width {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_WIDTH_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFoldGroup loop width {loop_width} does not equal declared width {declared_loop_width}"
                        ),
                    ));
                }
                let Some(&declared_loop_signed) = variable_signedness.get(loop_var) else {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_SIGNEDNESS_EXISTS",
                        node_id,
                        "ForFoldGroup loop variable signedness is absent from the semantic type table"
                            .to_string(),
                    ));
                };
                if *loop_signed != declared_loop_signed {
                    return Err(fail(
                        "FOR_FOLD_GROUP.LOOP_SIGNEDNESS_MATCHES_VARIABLE",
                        node_id,
                        format!(
                            "ForFoldGroup loop signedness {loop_signed} does not equal declared signedness {declared_loop_signed}"
                        ),
                    ));
                }
                for state in states {
                    verify_atom(
                        &state.target.id,
                        state.target.access,
                        "ForFoldGroup state target",
                        node_id,
                    )?;
                }
            }
            _ => {}
        }
    }

    for (path_index, path) in paths.iter().enumerate() {
        let expression_width = require(path.expr, "logic-path result")?;
        if let LogicPathTarget::Var(target) = &path.target {
            let target_width =
                verify_atom(&target.id, target.access, "logic-path target", path.expr)?;
            if expression_width != target_width {
                return Err(fail(
                    "ROOT.RESULT_WIDTH_MATCHES_TARGET",
                    path.expr,
                    format!(
                        "logic-path result width {expression_width} does not equal target width {target_width}"
                    ),
                ));
            }
        }
        for &node in &path.pre_lower_nodes {
            require(node, "logic-path pre-lower value")?;
        }
        let mut local_ids = HashSet::default();
        for (_, node) in &path.local_inputs {
            require(*node, "logic-path local input")?;
        }
        for (id, _) in &path.local_inputs {
            if !local_ids.insert(id.clone()) {
                return Err(fail(
                    "ROOT.LOCAL_INPUT_ID_UNIQUE",
                    path.expr,
                    "logic-path contains duplicate local-input IDs".to_string(),
                ));
            }
        }
        for source in path
            .sources
            .iter()
            .chain(&path.previous_sources)
            .chain(&path.address_sources)
        {
            verify_atom(&source.id, source.access, "logic-path source", path.expr)?;
        }
        for address in &path.address_sources {
            if !path
                .sources
                .iter()
                .any(|source| source.id == address.id && source.access.overlaps(&address.access))
            {
                return Err(fail(
                    "ROOT.ADDRESS_SOURCE_IS_CURRENT_SOURCE",
                    path.expr,
                    "logic-path address source is absent from current-value sources".to_string(),
                ));
            }
        }
        for &successor in &path.order_before {
            if successor.0 >= paths.len() {
                return Err(fail(
                    "ROOT.ORDER_EDGE_EXISTS",
                    path.expr,
                    format!(
                        "logic path {path_index} orders before missing path {}",
                        successor.0
                    ),
                ));
            }
            if successor.0 == path_index {
                return Err(fail(
                    "ROOT.ORDER_EDGE_NOT_SELF",
                    path.expr,
                    format!("logic path {path_index} contains a self ordering edge"),
                ));
            }
        }
        if let LogicPathTarget::CombCaptureEvent {
            guard,
            args,
            loop_runner,
            ..
        } = &path.target
        {
            if let Some(guard) = guard {
                require(*guard, "capture-event guard")?;
            }
            for &arg in args {
                require(arg, "capture-event argument")?;
            }
            if let Some(loop_runner) = loop_runner {
                require(*loop_runner, "capture-event loop runner")?;
            }
        }
    }

    for observer in observers {
        if let Some(guard) = observer.guard {
            require(guard, "observer guard")?;
        }
        for &arg in &observer.args {
            require(arg, "observer argument")?;
        }
        if let Some(loop_runner) = observer.loop_runner {
            require(loop_runner, "observer loop runner")?;
        }
        let mut local_ids = HashSet::default();
        for (id, node) in &observer.local_inputs {
            require(*node, "observer local input")?;
            if !local_ids.insert(id.clone()) {
                return Err(fail(
                    "ROOT.LOCAL_INPUT_ID_UNIQUE",
                    *node,
                    "observer contains duplicate local-input IDs".to_string(),
                ));
            }
        }
        let diagnostic_node = observer
            .guard
            .or(observer.loop_runner)
            .or_else(|| observer.args.first().copied())
            .unwrap_or(NodeId(0));
        for atom in observer
            .sensitivity
            .iter()
            .chain(&observer.observed_inputs)
            .chain(&observer.position_inputs)
            .chain(&observer.preceding_writes)
            .chain(&observer.written_before)
            .chain(&observer.written_input_atoms)
        {
            verify_atom(&atom.id, atom.access, "observer atom", diagnostic_node)?;
        }
        for id in &observer.written_inputs {
            if !variable_widths.contains_key(id) {
                return Err(fail(
                    "ROOT.VARIABLE_EXISTS",
                    diagnostic_node,
                    "observer written input is absent from the semantic type table".to_string(),
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod slt_root_verify_tests {
    use num_bigint::{BigInt, BigUint};

    use super::verify_slt_roots;
    use crate::ir::VarAtomBase;
    use crate::logic_tree::{
        LogicPath, LogicPathTarget, SLTForFoldGroupState, SLTForUpdate, SLTLoopBound, SLTNode,
        SLTNodeArena, SLTStepOp,
    };

    fn path(expr: crate::logic_tree::NodeId) -> LogicPath<u32> {
        LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(2, 0, 7)),
            sources: crate::HashSet::default(),
            previous_sources: crate::HashSet::default(),
            address_sources: crate::HashSet::default(),
            local_inputs: Vec::new(),
            order_before: crate::HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr,
        }
    }

    fn semantic_tables() -> (crate::HashMap<u32, usize>, crate::HashMap<u32, bool>) {
        (
            [(1, 8), (2, 8)].into_iter().collect(),
            [(1, false), (2, false)].into_iter().collect(),
        )
    }

    #[test]
    fn rejects_legacy_for_fold_loop_signedness_mismatch() {
        let mut arena = SLTNodeArena::new();
        let value = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let continue_cond = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let target = VarAtomBase::new(2, 0, 7);
        let fold = arena
            .alloc(SLTNode::ForFold {
                loop_var: 1,
                loop_width: 8,
                loop_signed: true,
                start: SLTLoopBound::Const(0),
                end: SLTLoopBound::Const(1),
                inclusive: false,
                step: 1,
                step_op: SLTStepOp::Add,
                reverse: false,
                result: target,
                initials: vec![SLTForUpdate {
                    target,
                    expr: value,
                }],
                updates: vec![SLTForUpdate {
                    target,
                    expr: value,
                }],
                effects: Vec::new(),
                continue_cond,
            })
            .unwrap();
        let (widths, signedness) = semantic_tables();

        let error =
            verify_slt_roots(&arena, &[path(fold)], &[], &widths, &signedness, "test").unwrap_err();
        let super::ParserError::SltVerify { error, .. } = error else {
            panic!("expected SLT verifier error")
        };
        assert_eq!(error.invariant, "FOR_FOLD.LOOP_SIGNEDNESS_MATCHES_VARIABLE");
    }

    #[test]
    fn rejects_for_fold_group_loop_signedness_mismatch() {
        let mut arena = SLTNodeArena::new();
        let value = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let guard = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let group = arena
            .alloc(SLTNode::ForFoldGroup {
                loop_var: 1,
                loop_width: 8,
                loop_signed: true,
                start: BigInt::from(0),
                step: BigInt::from(1),
                trip_count: 2,
                entry_guard: guard,
                states: vec![SLTForFoldGroupState {
                    target: VarAtomBase::new(2, 0, 7),
                    initial: value,
                    update: value,
                }],
            })
            .unwrap();
        let (widths, signedness) = semantic_tables();

        let error = verify_slt_roots(&arena, &[path(group)], &[], &widths, &signedness, "test")
            .unwrap_err();
        let super::ParserError::SltVerify { error, .. } = error else {
            panic!("expected SLT verifier error")
        };
        assert_eq!(
            error.invariant,
            "FOR_FOLD_GROUP.LOOP_SIGNEDNESS_MATCHES_VARIABLE"
        );
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BuildConfig {
    pub clock_type: ClockType,
    pub reset_type: ResetType,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            clock_type: ClockType::PosEdge,
            reset_type: ResetType::AsyncLow,
        }
    }
}

impl From<&veryl_metadata::Build> for BuildConfig {
    fn from(build: &veryl_metadata::Build) -> Self {
        Self {
            clock_type: build.clock_type,
            reset_type: build.reset_type,
        }
    }
}
pub mod bitaccess;
mod bitslicer;
pub(crate) mod case;
pub mod ff;
mod fold_group_fusion;
pub(crate) mod loop_provenance;
pub mod module;
pub mod registry;
mod scheduler;
use crate::ir::{
    AbsoluteAddr, CombObserver, DomainKind, ExecutionUnit, GlueAddr, InstanceId, InstancePath,
    LogicPathId, ModuleId, Program, RegionedAbsoluteAddr, RuntimeErrorInfo, STABLE_REGION,
    SimModule, VarAtomBase, VariableInfo,
};
use crate::logic_tree::{LogicPath, LogicPathTarget, NodeId, SLTNodeArena, SLTNodeFacts};
pub use scheduler::SchedulerError;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hash;
use veryl_analyzer::ir::Declaration;

/// Source location information for rich error diagnostics.
#[derive(Debug)]
pub struct SourceLocation {
    pub source: MultiSources,
    pub span: miette::SourceSpan,
}

impl SourceLocation {
    pub fn from_token(token: &TokenRange) -> Self {
        let path = token.beg.source.to_string();
        let text = token.beg.source.get_text();
        Self {
            source: MultiSources {
                sources: vec![Source { path, text }],
            },
            span: token.into(),
        }
    }

    fn path(&self) -> Option<&str> {
        self.source
            .sources
            .first()
            .map(|source| source.path.as_str())
    }
}

/// The compilation phase where an unsupported feature was encountered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoweringPhase {
    FfLowering,
    CombLowering,
    SimulatorParser,
}

impl std::fmt::Display for LoweringPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoweringPhase::FfLowering => write!(f, "FF lowering"),
            LoweringPhase::CombLowering => write!(f, "comb lowering"),
            LoweringPhase::SimulatorParser => write!(f, "simulator parser"),
        }
    }
}

#[derive(Error, Debug)]
pub enum ParserError {
    #[error(transparent)]
    Scheduler(SchedulerError<String>),

    #[error("{error}")]
    SchedulerWithLocation {
        error: SchedulerError<String>,
        source_locations: Vec<SourceLocation>,
    },

    #[error("Unsupported in {phase}: {feature} [tracking issue #{issue}] ({detail})")]
    Unsupported {
        issue: u32,
        phase: LoweringPhase,
        feature: &'static str,
        detail: String,
        source_location: Option<SourceLocation>,
    },

    #[error("Illegal in current context: {feature} ({detail})")]
    IllegalContext {
        feature: &'static str,
        detail: String,
        source_location: Option<SourceLocation>,
    },

    #[error(
        "Unresolved type width for variable `{variable}` in module `{module}`: \
             width cannot be determined at compile time (type: {typ})"
    )]
    UnresolvedWidth {
        module: String,
        variable: String,
        typ: String,
        source_location: Option<SourceLocation>,
    },

    #[error("Top module `{name}` not found in IR")]
    TopNotFound { name: String },

    #[error("Top module `{name}` is generic and cannot be used as a top-level module")]
    GenericTop { name: String },

    #[error("SIR verification failed {phase} in {group} unit {unit}: {error}")]
    SirVerify {
        phase: &'static str,
        group: &'static str,
        unit: usize,
        #[source]
        error: crate::ir::verify::SirVerifyError,
    },

    #[error("SLT verification failed {phase}: {error}")]
    SltVerify {
        phase: &'static str,
        #[source]
        error: crate::logic_tree::SLTNodeFactsError,
    },

    #[error("SLT construction failed: {0}")]
    SltConstruction(#[from] crate::logic_tree::SLTNodeFactsError),
}

impl ParserError {
    pub fn unsupported(
        issue: u32,
        phase: LoweringPhase,
        feature: &'static str,
        detail: impl Into<String>,
        token: Option<&TokenRange>,
    ) -> Self {
        ParserError::Unsupported {
            issue,
            phase,
            feature,
            detail: detail.into(),
            source_location: token.map(SourceLocation::from_token),
        }
    }

    pub fn illegal_context(
        feature: &'static str,
        detail: impl Into<String>,
        token: Option<&TokenRange>,
    ) -> Self {
        ParserError::IllegalContext {
            feature,
            detail: detail.into(),
            source_location: token.map(SourceLocation::from_token),
        }
    }

    pub fn unresolved_width(
        module: &veryl_analyzer::ir::Module,
        var: &veryl_analyzer::ir::Variable,
        typ: impl Into<String>,
    ) -> Self {
        ParserError::UnresolvedWidth {
            module: module.name.to_string(),
            variable: var.path.to_string(),
            typ: typ.into(),
            source_location: Some(SourceLocation::from_token(&var.token)),
        }
    }
}

impl miette::Diagnostic for ParserError {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        match self {
            ParserError::Unsupported { phase, .. } => Some(Box::new(format!(
                "unsupported_{}",
                match phase {
                    LoweringPhase::FfLowering => "ff_lowering",
                    LoweringPhase::CombLowering => "comb_lowering",
                    LoweringPhase::SimulatorParser => "simulator_parser",
                }
            ))),
            ParserError::IllegalContext { .. } => Some(Box::new("illegal_context")),
            ParserError::UnresolvedWidth { .. } => Some(Box::new("unresolved_width")),
            ParserError::Scheduler(_) | ParserError::SchedulerWithLocation { .. } => {
                Some(Box::new("scheduler"))
            }
            ParserError::TopNotFound { .. } => Some(Box::new("top_not_found")),
            ParserError::GenericTop { .. } => Some(Box::new("generic_top")),
            ParserError::SirVerify { .. } => Some(Box::new("sir_verify")),
            ParserError::SltVerify { .. } | ParserError::SltConstruction(_) => {
                Some(Box::new("slt_verify"))
            }
        }
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Error)
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        let loc = match self {
            ParserError::Unsupported {
                source_location, ..
            }
            | ParserError::IllegalContext {
                source_location, ..
            }
            | ParserError::UnresolvedWidth {
                source_location, ..
            } => source_location.as_ref(),
            ParserError::SchedulerWithLocation {
                source_locations, ..
            } => source_locations.first(),
            _ => None,
        };
        loc.map(|l| &l.source as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let loc = match self {
            ParserError::Unsupported {
                source_location, ..
            }
            | ParserError::IllegalContext {
                source_location, ..
            }
            | ParserError::UnresolvedWidth {
                source_location, ..
            } => source_location.as_ref(),
            _ => None,
        };
        if let Some(loc) = loc {
            return Some(Box::new(std::iter::once(
                miette::LabeledSpan::new_with_span(Some("Error location".to_string()), loc.span),
            )));
        }

        match self {
            ParserError::SchedulerWithLocation {
                source_locations, ..
            } => {
                let first_path = source_locations.first().and_then(SourceLocation::path)?;
                let labels = source_locations
                    .iter()
                    .filter(move |loc| loc.path() == Some(first_path))
                    .map(|loc| {
                        miette::LabeledSpan::new_with_span(
                            Some("loop participant".to_string()),
                            loc.span,
                        )
                    })
                    .collect::<Vec<_>>();
                if labels.is_empty() {
                    None
                } else {
                    Some(Box::new(labels.into_iter()))
                }
            }
            _ => None,
        }
    }
}

/// Resolve the total storage size of a variable (total_width * total_array),
/// returning `ParserError::UnresolvedWidth` when it cannot be determined.
pub(crate) fn resolve_total_width(
    module: &veryl_analyzer::ir::Module,
    var: &veryl_analyzer::ir::Variable,
) -> Result<usize, ParserError> {
    let width = var
        .total_width()
        .ok_or_else(|| ParserError::unresolved_width(module, var, var.r#type.to_string()))?;
    let array = var
        .r#type
        .total_array()
        .ok_or_else(|| ParserError::unresolved_width(module, var, var.r#type.to_string()))?;
    Ok(width * array)
}

/// Resolve each dimension in an array/width shape, returning an error when any is `None`.
pub(crate) fn resolve_dims(
    module: &veryl_analyzer::ir::Module,
    var: &veryl_analyzer::ir::Variable,
    shape: &[Option<usize>],
    kind: &str,
) -> Result<Vec<usize>, ParserError> {
    shape
        .iter()
        .map(|d| {
            d.ok_or_else(|| {
                ParserError::unresolved_width(
                    module,
                    var,
                    format!("{} dimension in {}", kind, var.r#type),
                )
            })
        })
        .collect()
}

pub struct ParseIrResult<'a> {
    pub modules: HashMap<ModuleId, SimModule>,
    pub module_ir: HashMap<ModuleId, &'a Module>,
    pub module_names: HashMap<ModuleId, StrId>,
    pub root_id: ModuleId,
}

#[cfg(test)]
pub fn parse_ir<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    config: &BuildConfig,
    top: &StrId,
) -> Result<ParseIrResult<'a>, ParserError> {
    parse_ir_with_loop_provenance(ir, &loop_provenance::LoopProvenance::default(), config, top)
}

fn parse_ir_with_loop_provenance<'a>(
    ir: &'a veryl_analyzer::ir::Ir,
    loop_provenance: &loop_provenance::LoopProvenance,
    config: &BuildConfig,
    top: &StrId,
) -> Result<ParseIrResult<'a>, ParserError> {
    // Pre-step: build name_to_ir and generic_names
    let mut name_to_ir: HashMap<StrId, &'a Module> = HashMap::default();
    let mut generic_names: HashSet<StrId> = HashSet::default();
    for component in &ir.components {
        match component {
            Component::Module(module) => {
                let is_generic = module.variables.values().any(|v| v.r#type.is_unknown());
                if is_generic {
                    generic_names.insert(module.name);
                }
                name_to_ir.insert(module.name, module);
            }
            Component::Interface(_) => {
                unreachable!("Interface component must be eliminated before simulator parse_ir")
            }
            Component::SystemVerilog(sv) => {
                return Err(ParserError::unsupported(
                    64,
                    LoweringPhase::SimulatorParser,
                    "systemverilog component",
                    format!("name: \"{}\"", sv.name),
                    None,
                ));
            }
        }
    }

    let mut modules: HashMap<ModuleId, SimModule> = HashMap::default();
    let mut module_ir: HashMap<ModuleId, &'a Module> = HashMap::default();
    let mut module_names: HashMap<ModuleId, StrId> = HashMap::default();
    let mut name_to_id: HashMap<StrId, ModuleId> = HashMap::default();
    let mut next_id: usize = 0;

    // Allocate root
    let root_id = ModuleId(next_id);
    next_id += 1;
    let root_ir = name_to_ir
        .get(top)
        .ok_or_else(|| ParserError::TopNotFound {
            name: resource_table::get_str_value(*top).unwrap_or_default(),
        })?;
    if generic_names.contains(top) {
        return Err(ParserError::GenericTop {
            name: resource_table::get_str_value(*top).unwrap_or_default(),
        });
    }
    name_to_id.insert(*top, root_id);
    module_names.insert(root_id, *top);
    module_ir.insert(root_id, root_ir);

    // Worklist: (my_id, ir_module)
    let mut worklist: Vec<(ModuleId, &'a Module)> = vec![(root_id, root_ir)];
    // inst_id sequences per module (for ModuleParser)
    let mut inst_sequences: HashMap<ModuleId, Vec<ModuleId>> = HashMap::default();

    let mut i = 0;
    while i < worklist.len() {
        let (my_id, ir_module) = worklist[i];
        i += 1;

        let mut inst_ids = Vec::new();
        for decl in &ir_module.declarations {
            if let Declaration::Inst(inst_decl) = decl {
                match &*inst_decl.component {
                    Component::SystemVerilog(_) => {
                        // SV modules: allocate a placeholder ModuleId.
                        // ModuleParser::parse_inst_declaration will return an error.
                        let child_id = ModuleId(next_id);
                        next_id += 1;
                        inst_ids.push(child_id);
                    }
                    Component::Module(child_module) => {
                        let child_name = child_module.name;
                        let has_params = child_module
                            .variables
                            .values()
                            .any(|v| v.kind == VarKind::Param);
                        if generic_names.contains(&child_name) || has_params {
                            // Generic or parametric: each inst gets a unique concrete module
                            let child_id = ModuleId(next_id);
                            next_id += 1;
                            module_names.insert(child_id, child_name);
                            module_ir.insert(child_id, child_module);
                            worklist.push((child_id, child_module));
                            inst_ids.push(child_id);
                        } else {
                            // Non-generic, non-parametric: dedup by name
                            let child_id = if let Some(&existing) = name_to_id.get(&child_name) {
                                existing
                            } else {
                                let id = ModuleId(next_id);
                                next_id += 1;
                                name_to_id.insert(child_name, id);
                                module_names.insert(id, child_name);
                                module_ir.insert(id, child_module);
                                worklist.push((id, child_module));
                                id
                            };
                            inst_ids.push(child_id);
                        }
                    }
                    Component::Interface(_) => {
                        unreachable!("Interface component in inst declaration")
                    }
                }
            }
        }
        inst_sequences.insert(my_id, inst_ids);
    }

    // Parse all discovered modules
    for (mid, ir_module) in &module_ir {
        let inst_ids = inst_sequences.get(mid).map(|v| v.as_slice()).unwrap_or(&[]);
        let sim_module =
            ModuleParser::parse_with_loop_provenance(ir_module, loop_provenance, config, inst_ids)?;
        modules.insert(*mid, sim_module);
    }

    Ok(ParseIrResult {
        modules,
        module_ir,
        module_names,
        root_id,
    })
}

fn create_absolute_addr(
    instance_path: &[(String, usize)],
    var_path: &[String],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> AbsoluteAddr {
    let instance_path = InstancePath(
        instance_path
            .iter()
            .map(|s| (resource_table::insert_str(&s.0), s.1))
            .collect(),
    );
    let instance_id = expanded[&instance_path];
    let module_id = instance_modules[&instance_id];
    let module = &modules[&module_id];
    let var_path = VarPath(
        var_path
            .iter()
            .map(|s| resource_table::insert_str(s))
            .collect(),
    );
    let var_id = *module
        .variables
        .iter()
        .find(|v| v.1.path == var_path)
        .unwrap()
        .0;
    AbsoluteAddr {
        instance_id,
        var_id,
    }
}
fn parse_ignored_loops(
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashSet<(AbsoluteAddr, AbsoluteAddr)> {
    let mut res = HashSet::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path)) in ignored_loops {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to));
    }
    res
}
fn parse_true_loops(
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &HashMap<InstancePath, InstanceId>,
) -> HashMap<(AbsoluteAddr, AbsoluteAddr), usize> {
    let mut res = HashMap::default();

    for ((from_instance_path, from_var_path), (to_instance_path, to_var_path), max_iter) in
        true_loops
    {
        let from = create_absolute_addr(
            from_instance_path,
            from_var_path,
            instance_modules,
            modules,
            expanded,
        );
        let to = create_absolute_addr(
            to_instance_path,
            to_var_path,
            instance_modules,
            modules,
            expanded,
        );
        res.insert((from, to), *max_iter);
    }
    res
}

fn scheduler_source_locations(
    error: &SchedulerError<AbsoluteAddr>,
    module_ir: &HashMap<ModuleId, &Module>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
) -> Vec<SourceLocation> {
    let blocks = match error {
        SchedulerError::CombinationalLoop { blocks } => blocks,
        SchedulerError::MultipleDriver { blocks } => blocks,
    };
    let mut seen = HashSet::default();
    blocks
        .iter()
        .filter_map(|block| {
            let addr = block.target.var()?.id;
            if !seen.insert(addr) {
                return None;
            }
            let module_id = instance_modules.get(&addr.instance_id)?;
            let module = module_ir.get(module_id)?;
            let var = module.variables.get(&addr.var_id)?;
            Some(SourceLocation::from_token(&var.token))
        })
        .collect()
}

pub(crate) fn flatten(
    root_id: &ModuleId,
    module_ir: &HashMap<ModuleId, &Module>,
    modules: HashMap<ModuleId, SimModule>,
    module_names: HashMap<ModuleId, StrId>,
    config: &BuildConfig,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
) -> Result<Program, ParserError> {
    let flatten_timing = std::env::var("CELOX_PHASE_TIMING").is_ok();
    macro_rules! timed_sub {
        ($label:expr, $body:expr) => {{
            if flatten_timing {
                let start = crate::timing::now();
                let result = $body;
                eprintln!("[flatten] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.sim_modules
    {
        t.sim_modules = Some(modules.clone());
    }

    let (expanded, instance_modules) =
        timed_sub!("expand_hierarchy", expand_hierarchy(root_id, &modules));
    let global_boundaries = timed_sub!(
        "propagate_boundaries",
        propagate_boundaries(&expanded, &instance_modules, &modules)
    );

    let clock_domains = timed_sub!(
        "unify_clock_domains",
        unify_clock_domains(&expanded, &instance_modules, &modules)
    );
    let (
        mut global_arena,
        mut eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        mut comb_blocks,
        mut comb_observers,
        mut runtime_errors,
        runtime_event_sites,
        next_runtime_error_code,
    ) = timed_sub!(
        "relocate_units",
        relocate_units(
            &expanded,
            &instance_modules,
            &modules,
            &global_boundaries,
            &clock_domains,
            trace_opts,
            &mut trace,
        )
    )?;
    let ignored_loops = parse_ignored_loops(ignored_loops, &instance_modules, &modules, &expanded);
    let true_loops = parse_true_loops(true_loops, &instance_modules, &modules, &expanded);

    // Build reset -> clock mapping with AbsoluteAddr
    let mut reset_clock_map: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (reset_var_id, clock_var_id) in &sim_module.reset_clock_map {
            let reset_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *reset_var_id,
            };
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *clock_var_id,
            };
            // Use canonical clock domain if available
            let canonical_clock = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            let canonical_reset = clock_domains
                .get(&reset_addr)
                .copied()
                .unwrap_or(reset_addr);
            reset_clock_map.insert(canonical_reset, canonical_clock);
        }
    }

    let (topological_clocks, cascaded_clocks) = timed_sub!(
        "analyze_clock_dependencies",
        analyze_clock_dependencies(
            &mut eval_apply_ffs,
            &comb_blocks,
            &global_arena,
            &clock_domains,
            &expanded,
            &instance_modules,
            &modules,
            config,
        )
    );

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.flattened_comb_blocks
    {
        t.flattened_comb_blocks = Some((comb_blocks.clone(), global_arena.clone()));
    }

    // Constant variable inlining: detect variables whose every LogicPath
    // is a constant, then replace all Input references with Constant nodes.
    // This eliminates Store→Load roundtrips for compile-time constants
    // (e.g. genvar-expanded parity-check matrices).
    crate::logic_tree::const_inline::inline_constant_variables(
        &mut comb_blocks,
        &mut global_arena,
    )?;
    apply_always_comb_previous_source_ordering(&mut comb_blocks);

    let var_widths: HashMap<AbsoluteAddr, usize> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            let module = module_ir[&mod_id];
            module.variables.iter().filter_map(move |(var_id, var)| {
                resolve_total_width(module, var).ok().map(|w| {
                    (
                        AbsoluteAddr {
                            instance_id: inst_id,
                            var_id: *var_id,
                        },
                        w,
                    )
                })
            })
        })
        .collect();
    let var_signedness: HashMap<AbsoluteAddr, bool> = instance_modules
        .iter()
        .flat_map(|(&inst_id, &mod_id)| {
            module_ir[&mod_id]
                .variables
                .iter()
                .map(move |(var_id, var)| {
                    (
                        AbsoluteAddr {
                            instance_id: inst_id,
                            var_id: *var_id,
                        },
                        var.r#type.signed,
                    )
                })
        })
        .collect();

    build_comb_observer_capture_paths(
        &mut comb_blocks,
        &mut comb_observers,
        &runtime_event_sites,
        &mut global_arena,
    )?;
    for (site_id, site) in runtime_event_sites.iter().enumerate() {
        if !matches!(site.kind, crate::ir::RuntimeEventKind::AssertFatal) {
            continue;
        }
        runtime_errors
            .entry(site_id as i64)
            .or_insert_with(|| crate::ir::RuntimeErrorInfo {
                message: site
                    .template
                    .clone()
                    .unwrap_or_else(|| "assertion failed".to_string()),
                signals: Vec::new(),
            });
    }

    verify_slt_roots(
        &global_arena,
        &comb_blocks,
        &comb_observers,
        &var_widths,
        &var_signedness,
        "after flattening symbolic logic",
    )?;

    let sched_start = flatten_timing.then(crate::timing::now);
    let schedule = match scheduler::sort(
        comb_blocks,
        &global_arena,
        &ignored_loops,
        &true_loops,
        four_state,
        &var_widths,
        next_runtime_error_code,
    ) {
        Ok(schedule) => schedule,
        Err(error) => {
            let (err_vars, err_path_idx) = module_variables(module_ir, config).unwrap_or_default();
            let program = Program {
                eval_apply_ffs: HashMap::default(),
                eval_only_ffs: HashMap::default(),
                apply_ffs: HashMap::default(),
                eval_comb: Vec::new(),
                runtime_errors: HashMap::default(),
                runtime_event_sites: Vec::new(),
                comb_observers: Vec::new(),
                eval_comb_plan: None,
                instance_ids: expanded.clone(),
                instance_module: instance_modules.clone(),
                module_variables: err_vars,
                module_var_path_index: err_path_idx,
                module_names: module_names.clone(),
                clock_domains: HashMap::default(),
                topological_clocks: Vec::new(),
                cascaded_clocks: BTreeSet::new(),
                arena: SLTNodeArena::new(),
                num_events: 0,
                reset_clock_map: HashMap::default(),
                address_aliases: HashMap::default(),
                layout: None,
                initial_memory_values: Vec::new(),
                initial_statements: None,
                tb_functions: fxhash::FxHashMap::default(),
            };
            let source_locations = scheduler_source_locations(&error, module_ir, &instance_modules);
            let mut target_arena = SLTNodeArena::new();
            let error = error.map_addr(&global_arena, &mut target_arena, &|addr| {
                program.get_path(addr)
            })?;
            return Err(if source_locations.is_empty() {
                ParserError::Scheduler(error)
            } else {
                ParserError::SchedulerWithLocation {
                    error,
                    source_locations,
                }
            });
        }
    };
    if let Some(s) = sched_start {
        eprintln!("[flatten] scheduler::sort: {:?}", s.elapsed());
    }
    runtime_errors.extend(schedule.runtime_errors);
    let schduled: Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>> = schedule
        .execution_units
        .into_iter()
        .map(|eu| crate::ir::ExecutionUnit {
            entry_block_id: eu.entry_block_id,
            blocks: eu
                .blocks
                .into_iter()
                .map(|(id, bb)| {
                    (
                        id,
                        crate::ir::BasicBlock {
                            id: bb.id,
                            params: bb.params,
                            instructions: bb
                                .instructions
                                .into_iter()
                                .map(|inst| {
                                    inst.into_map_addr(|addr| RegionedAbsoluteAddr {
                                        region: STABLE_REGION,
                                        instance_id: addr.instance_id,
                                        var_id: addr.var_id,
                                    })
                                })
                                .collect(),
                            terminator: bb.terminator,
                        },
                    )
                })
                .collect(),
            register_map: eu.register_map,
        })
        .collect();
    let eval_comb = schduled.clone();

    if let Some(t) = trace
        && trace_opts.scheduled_units
    {
        t.scheduled_units = Some(schduled.clone());
    }

    // Conditional Population: only include split blocks if multiple active FF domains exist.
    // This optimization saves JIT resources for simple designs and designs where only one clock is active.
    let active_ff_domains = eval_apply_ffs
        .values()
        .filter(|eus| !eus.is_empty())
        .count();

    let (eval_only_ffs, apply_ffs) = if active_ff_domains > 1 {
        (eval_only_ffs, apply_ffs)
    } else {
        (HashMap::default(), HashMap::default())
    };

    // Extract initial block statements from root module (for native testbenches)
    let initial_statements = module_ir.get(root_id).and_then(|root_module| {
        let mut stmts = Vec::new();
        for decl in &root_module.declarations {
            if let Declaration::Initial(init_decl) = decl {
                stmts.extend(init_decl.statements.iter().cloned());
            }
        }
        if stmts.is_empty() { None } else { Some(stmts) }
    });

    let num_events = topological_clocks.len();
    let (mod_vars, mod_path_idx) = module_variables(module_ir, config)?;
    let initial_memory_values = instance_modules
        .iter()
        .flat_map(|(&instance_id, module_id)| {
            modules[module_id]
                .initial_memory_values
                .iter()
                .map(move |init| crate::ir::InitialMemoryValue {
                    addr: AbsoluteAddr {
                        instance_id,
                        var_id: init.var_id,
                    },
                    data: init.data.clone(),
                })
        })
        .collect();
    let program = Program {
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        eval_comb,
        runtime_errors,
        runtime_event_sites,
        comb_observers,
        eval_comb_plan: None,
        instance_ids: expanded,
        instance_module: instance_modules,
        module_variables: mod_vars,
        module_var_path_index: mod_path_idx,
        module_names,
        clock_domains,
        topological_clocks,
        cascaded_clocks,
        arena: global_arena,
        num_events,
        reset_clock_map,
        address_aliases: HashMap::default(),
        layout: None,
        initial_memory_values,
        initial_statements,
        tb_functions: module_ir
            .get(root_id)
            .map(|m| m.functions.clone())
            .unwrap_or_default(),
    };

    // --- Trigger Injection ---
    let mut trigger_map: HashMap<AbsoluteAddr, Vec<crate::ir::TriggerIdWithKind>> =
        HashMap::default();
    let module_vars = &program.module_variables;
    for (id, addr) in program.topological_clocks.iter().enumerate() {
        if let Some(module_id) = program.instance_module.get(&addr.instance_id) {
            if let Some(vars) = module_vars.get(module_id) {
                // Find variable info by var_id
                if let Some(info) = vars.get(&addr.var_id) {
                    let kind = info.kind;
                    trigger_map
                        .entry(*addr)
                        .or_default()
                        .push(crate::ir::TriggerIdWithKind { kind, id });
                }
            }
        }
    }

    let mut program = program;
    for units in program.eval_apply_ffs.values_mut() {
        for eu in units {
            for bb in eu.blocks.values_mut() {
                for inst in &mut bb.instructions {
                    match inst {
                        crate::ir::SIRInstruction::Store(addr, _, _, _, triggers, _) => {
                            let abs = addr.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        crate::ir::SIRInstruction::Commit(_, dst, .., triggers) => {
                            let abs = dst.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for units in program.eval_only_ffs.values_mut() {
        for eu in units {
            for bb in eu.blocks.values_mut() {
                for inst in &mut bb.instructions {
                    match inst {
                        crate::ir::SIRInstruction::Store(addr, _, _, _, triggers, _) => {
                            let abs = addr.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        crate::ir::SIRInstruction::Commit(_, dst, .., triggers) => {
                            let abs = dst.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                *triggers = ts.clone();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for units in program.apply_ffs.values_mut() {
        for eu in units {
            for bb in eu.blocks.values_mut() {
                for inst in &mut bb.instructions {
                    match inst {
                        crate::ir::SIRInstruction::Store(addr, ..) => {
                            let abs = addr.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                if let crate::ir::SIRInstruction::Store(_, _, _, _, triggers, _) =
                                    inst
                                {
                                    *triggers = ts.clone();
                                }
                            }
                        }
                        crate::ir::SIRInstruction::Commit(_, dst, ..) => {
                            let abs = dst.absolute_addr();
                            let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                            if let Some(ts) = trigger_map.get(&canonical) {
                                if let crate::ir::SIRInstruction::Commit(.., triggers) = inst {
                                    *triggers = ts.clone();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    for eu in &mut program.eval_comb {
        for bb in eu.blocks.values_mut() {
            for inst in &mut bb.instructions {
                match inst {
                    crate::ir::SIRInstruction::Store(addr, _, _, _, triggers, _) => {
                        let abs = addr.absolute_addr();
                        let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                        if let Some(ts) = trigger_map.get(&canonical) {
                            *triggers = ts.clone();
                        }
                    }
                    crate::ir::SIRInstruction::Commit(_, dst, .., triggers) => {
                        let abs = dst.absolute_addr();
                        let canonical = program.clock_domains.get(&abs).copied().unwrap_or(abs);
                        if let Some(ts) = trigger_map.get(&canonical) {
                            *triggers = ts.clone();
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    dump_addr_map_if_requested(&program);

    Ok(program)
}

fn dump_addr_map_if_requested(program: &Program) {
    if std::env::var_os("CELOX_ADDR_MAP_DUMP").is_none() {
        return;
    }

    let filter = parse_addr_map_filter();
    let mut entries = Vec::new();
    for (&instance_id, &module_id) in &program.instance_module {
        let Some(vars) = program.module_variables.get(&module_id) else {
            continue;
        };
        for (&var_id, info) in vars {
            let inst_key = instance_id.0.to_string();
            let var_key = normalized_addr_id(&var_id.to_string());
            if let Some(filter) = &filter
                && !filter.contains(&(inst_key, var_key))
            {
                continue;
            }
            entries.push((instance_id, module_id, var_id, info));
        }
    }

    entries.sort_by(|(a_inst, _, a_var, _), (b_inst, _, b_var, _)| {
        (a_inst.0, a_var.to_string()).cmp(&(b_inst.0, b_var.to_string()))
    });

    for (instance_id, module_id, var_id, info) in entries {
        let module_name = program
            .module_names
            .get(&module_id)
            .and_then(|name| resource_table::get_str_value(*name))
            .unwrap_or_default();
        let addr = AbsoluteAddr {
            instance_id,
            var_id,
        };
        eprintln!(
            "[addr-map] inst={} var={} module={} path={} width={} array_dims={:?} 4state={} kind={:?} var_kind={}",
            instance_id,
            var_id,
            module_name,
            program.get_path(&addr),
            info.width,
            info.array_dims,
            info.is_4state,
            info.kind,
            info.var_kind.description(),
        );
    }
}

fn parse_addr_map_filter() -> Option<HashSet<(String, String)>> {
    let raw = std::env::var_os("CELOX_ADDR_MAP_FILTER")?;
    let raw = raw.to_string_lossy();
    let mut filter = HashSet::default();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let Some((inst, var)) = item.split_once(':') else {
            continue;
        };
        filter.insert((normalized_addr_id(inst), normalized_addr_id(var)));
    }
    Some(filter)
}

fn normalized_addr_id(raw: &str) -> String {
    raw.trim()
        .trim_start_matches("inst")
        .trim_start_matches("var")
        .to_string()
}

fn module_variables(
    module_ir: &HashMap<ModuleId, &Module>,
    config: &BuildConfig,
) -> Result<
    (
        HashMap<ModuleId, HashMap<VarId, VariableInfo>>,
        HashMap<ModuleId, HashMap<VarPath, Option<VarId>>>,
    ),
    ParserError,
> {
    let mut res = HashMap::default();
    let mut path_index = HashMap::default();
    for (id, module) in module_ir {
        let mut variables = HashMap::default();
        let mut paths: HashMap<VarPath, Option<VarId>> = HashMap::default();
        for (id, varibale) in &module.variables {
            match paths.entry(varibale.path.clone()) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(Some(*id));
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    // Duplicate VarPath — mark as ambiguous
                    e.insert(None);
                }
            }
            variables.insert(
                *id,
                VariableInfo {
                    width: resolve_total_width(module, varibale)?,
                    id: *id,
                    path: varibale.path.clone(),
                    is_4state: is_4state_type(&varibale.r#type.kind),
                    kind: type_kind_to_domain_kind(&varibale.r#type.kind, config),
                    var_kind: varibale.kind,
                    type_kind: type_kind_to_port_type_kind(&varibale.r#type.kind, config),
                    array_dims: varibale.r#type.array.iter().filter_map(|d| *d).collect(),
                },
            );
        }
        res.insert(*id, variables);
        path_index.insert(*id, paths);
    }
    Ok((res, path_index))
}

fn type_kind_to_port_type_kind(
    kind: &veryl_analyzer::ir::TypeKind,
    config: &BuildConfig,
) -> crate::ir::PortTypeKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock | TypeKind::ClockPosedge | TypeKind::ClockNegedge => {
            crate::ir::PortTypeKind::Clock
        }
        TypeKind::Reset => match config.reset_type {
            ResetType::AsyncHigh => crate::ir::PortTypeKind::ResetAsyncHigh,
            ResetType::AsyncLow => crate::ir::PortTypeKind::ResetAsyncLow,
            ResetType::SyncHigh => crate::ir::PortTypeKind::ResetSyncHigh,
            ResetType::SyncLow => crate::ir::PortTypeKind::ResetSyncLow,
        },
        TypeKind::ResetAsyncHigh => crate::ir::PortTypeKind::ResetAsyncHigh,
        TypeKind::ResetAsyncLow => crate::ir::PortTypeKind::ResetAsyncLow,
        TypeKind::ResetSyncHigh => crate::ir::PortTypeKind::ResetSyncHigh,
        TypeKind::ResetSyncLow => crate::ir::PortTypeKind::ResetSyncLow,
        TypeKind::Logic => crate::ir::PortTypeKind::Logic,
        TypeKind::Bit => crate::ir::PortTypeKind::Bit,
        _ => crate::ir::PortTypeKind::Other,
    }
}

fn type_kind_to_domain_kind(
    kind: &veryl_analyzer::ir::TypeKind,
    config: &BuildConfig,
) -> DomainKind {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock => match config.clock_type {
            ClockType::PosEdge => DomainKind::ClockPosedge,
            ClockType::NegEdge => DomainKind::ClockNegedge,
        },
        TypeKind::ClockPosedge => DomainKind::ClockPosedge,
        TypeKind::ClockNegedge => DomainKind::ClockNegedge,
        TypeKind::Reset => match config.reset_type {
            ResetType::AsyncHigh => DomainKind::ResetAsyncHigh,
            ResetType::AsyncLow => DomainKind::ResetAsyncLow,
            ResetType::SyncHigh | ResetType::SyncLow => DomainKind::Other,
        },
        TypeKind::ResetAsyncHigh => DomainKind::ResetAsyncHigh,
        TypeKind::ResetAsyncLow => DomainKind::ResetAsyncLow,
        _ => DomainKind::Other,
    }
}

fn is_4state_type(kind: &veryl_analyzer::ir::TypeKind) -> bool {
    use veryl_analyzer::ir::TypeKind;
    match kind {
        TypeKind::Clock
        | TypeKind::ClockPosedge
        | TypeKind::ClockNegedge
        | TypeKind::Reset
        | TypeKind::ResetAsyncHigh
        | TypeKind::ResetAsyncLow
        | TypeKind::ResetSyncHigh
        | TypeKind::ResetSyncLow
        | TypeKind::Logic => true,
        TypeKind::Struct(x) => x.members.iter().any(|m| is_4state_type(&m.r#type.kind)),
        TypeKind::Union(x) => x.members.iter().any(|m| is_4state_type(&m.r#type.kind)),
        TypeKind::Enum(x) => is_4state_type(&x.r#type.kind),
        _ => false,
    }
}

fn expand_hierarchy(
    top: &ModuleId,
    modules: &HashMap<ModuleId, SimModule>,
) -> (
    HashMap<InstancePath, InstanceId>,
    HashMap<InstanceId, ModuleId>,
) {
    let mut expanded = HashMap::default();
    let mut instance_modules = HashMap::default();
    let mut instance_id = 0;
    let path = vec![];
    let id = InstanceId(instance_id);
    instance_modules.insert(id, *top);
    expanded.insert(InstancePath(path.clone()), id);
    instance_id += 1;
    expand(
        top,
        path,
        modules,
        &mut expanded,
        &mut instance_modules,
        &mut instance_id,
    );
    (expanded, instance_modules)
}

fn propagate_boundaries(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>> {
    let mut current_boundaries = HashMap::default();

    // Initialize with local boundaries
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, boundaries) in &sim_module.comb_boundaries {
            let addr = AbsoluteAddr {
                instance_id: *id,
                var_id: *var_id,
            };
            current_boundaries.insert(addr, boundaries.clone());
        }
    }

    // Propagate boundaries
    let mut changed = true;
    while changed {
        changed = false;
        for (path, id) in expanded {
            let module_id = &instance_modules[id];
            let sim_module = &modules[module_id];

            for (inst_name, glue_blocks) in &sim_module.glue_blocks {
                for (idx, glue_block) in glue_blocks.iter().enumerate() {
                    let mut child_path = path.0.clone();
                    child_path.push((*inst_name, idx));
                    let child_id = expanded[&InstancePath(child_path)];

                    // Propagate from Parent to Child (Input Ports)
                    for (parent_vars, child_addr) in &glue_block.input_ports {
                        if let Some(target) = child_addr.target.var()
                            && let GlueAddr::Child(child_var_id) = target.id
                        {
                            let child_abs = AbsoluteAddr {
                                instance_id: child_id,
                                var_id: child_var_id,
                            };

                            // Collect boundaries from all parent variables connected to this port
                            let mut incoming_boundaries = std::collections::BTreeSet::new();
                            for parent_var in parent_vars {
                                let parent_abs = AbsoluteAddr {
                                    instance_id: *id,
                                    var_id: *parent_var,
                                };
                                if let Some(bounds) = current_boundaries.get(&parent_abs) {
                                    for b in bounds {
                                        incoming_boundaries.insert(*b);
                                    }
                                }
                            }

                            // Apply to child
                            if !incoming_boundaries.is_empty() {
                                let child_bounds = current_boundaries.entry(child_abs).or_default();
                                let old_len = child_bounds.len();
                                child_bounds.extend(incoming_boundaries);
                                if child_bounds.len() != old_len {
                                    changed = true;
                                }
                            }
                        }
                    }

                    // Propagate from Child to Parent (Output Ports)
                    for (parent_vars, logic_path) in &glue_block.output_ports {
                        // logic_path.target is Parent. logic_path.sources contains Child.
                        for source in &logic_path.sources {
                            if let GlueAddr::Child(child_var_id) = source.id {
                                let child_abs = AbsoluteAddr {
                                    instance_id: child_id,
                                    var_id: child_var_id,
                                };

                                // Child -> Parent
                                if let Some(child_bounds) =
                                    current_boundaries.get(&child_abs).cloned()
                                {
                                    for parent_var in parent_vars {
                                        let parent_abs = AbsoluteAddr {
                                            instance_id: *id,
                                            var_id: *parent_var,
                                        };
                                        let parent_bounds =
                                            current_boundaries.entry(parent_abs).or_default();
                                        let old_len = parent_bounds.len();
                                        parent_bounds.extend(child_bounds.clone());
                                        if parent_bounds.len() != old_len {
                                            changed = true;
                                        }
                                    }
                                }

                                // Parent -> Child (Sink -> Source propagation)
                                // If the parent wire connected to this output has boundaries (e.g. used in slices),
                                // those boundaries should propagate to the child output port so it drives them appropriately.
                                let mut incoming_boundaries = std::collections::BTreeSet::new();
                                for parent_var in parent_vars {
                                    let parent_abs = AbsoluteAddr {
                                        instance_id: *id,
                                        var_id: *parent_var,
                                    };
                                    if let Some(bounds) = current_boundaries.get(&parent_abs) {
                                        for b in bounds {
                                            incoming_boundaries.insert(*b);
                                        }
                                    }
                                }

                                if !incoming_boundaries.is_empty() {
                                    let child_bounds =
                                        current_boundaries.entry(child_abs).or_default();
                                    let old_len = child_bounds.len();
                                    child_bounds.extend(incoming_boundaries);
                                    if child_bounds.len() != old_len {
                                        changed = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    current_boundaries
}

fn expand(
    target: &ModuleId,
    path: Vec<(StrId, usize)>,
    modules: &HashMap<ModuleId, SimModule>,
    expanded: &mut HashMap<InstancePath, InstanceId>,
    instance_modules: &mut HashMap<InstanceId, ModuleId>,
    instance_id: &mut usize,
) {
    let module = &modules[target];
    for (inst_name, gbs) in &module.glue_blocks {
        for (idx, gb) in gbs.iter().enumerate() {
            let mut path = path.clone();
            path.push((*inst_name, idx));
            let id = InstanceId(*instance_id);
            expanded.insert(InstancePath(path.clone()), id);
            instance_modules.insert(id, gb.module_id);
            *instance_id += 1;
            expand(
                &gb.module_id,
                path,
                modules,
                expanded,
                instance_modules,
                instance_id,
            );
        }
    }
}

fn verify_program_sir(program: &Program, phase: &'static str) -> Result<(), ParserError> {
    let units = program
        .eval_comb
        .iter()
        .enumerate()
        .map(|(unit, eu)| ("eval_comb", unit, eu))
        .chain(
            program
                .eval_apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_apply_ffs", unit, eu)),
        )
        .chain(
            program
                .eval_only_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("eval_only_ffs", unit, eu)),
        )
        .chain(
            program
                .apply_ffs
                .values()
                .flatten()
                .enumerate()
                .map(|(unit, eu)| ("apply_ffs", unit, eu)),
        );
    for (group, unit, eu) in units {
        eu.verify_result().map_err(|error| ParserError::SirVerify {
            phase,
            group,
            unit,
            error,
        })?;
    }
    Ok(())
}

pub fn parse(
    top: &StrId,
    ir: &veryl_analyzer::ir::Ir,
    loop_provenance: &loop_provenance::LoopProvenance,
    config: &BuildConfig,
    ignored_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
    )],
    true_loops: &[(
        (Vec<(String, usize)>, Vec<String>),
        (Vec<(String, usize)>, Vec<String>),
        usize,
    )],
    four_state: bool,
    trace_opts: &crate::debug::TraceOptions,
    mut trace: Option<&mut crate::debug::CompilationTrace>,
    optimize_options: &crate::optimizer::OptimizeOptions,
) -> Result<Program, ParserError> {
    debug_assert!(
        loop_provenance.is_consistent_with(ir),
        "loop provenance must describe the analyzer IR passed to the parser"
    );
    let phase_timing = std::env::var("CELOX_PHASE_TIMING").is_ok();

    macro_rules! timed_phase {
        ($label:expr, $body:expr) => {{
            if phase_timing {
                let start = crate::timing::now();
                let result = $body;
                eprintln!("[phase-timing] {}: {:?}", $label, start.elapsed());
                result
            } else {
                $body
            }
        }};
    }

    let result = timed_phase!(
        "parse_ir",
        parse_ir_with_loop_provenance(ir, loop_provenance, config, top)
    )?;
    if let Some(t) = trace.as_deref_mut()
        && trace_opts.analyzer_ir
    {
        t.analyzer_ir = Some(ir.to_string());
    }
    let mut program = timed_phase!(
        "flatten",
        flatten(
            &result.root_id,
            &result.module_ir,
            result.modules,
            result.module_names,
            config,
            ignored_loops,
            true_loops,
            four_state,
            trace_opts,
            trace.as_deref_mut(),
        )
    )?;

    if let Some(t) = trace.as_deref_mut()
        && trace_opts.pre_optimized_sir
    {
        t.pre_optimized_sir = Some(program.clone());
    }

    timed_phase!(
        "verify_sir_before_optimize",
        verify_program_sir(&program, "before optimization")
    )?;

    // Always run the optimizer — even at O0, individual passes (e.g. TailCallSplit)
    // may be enabled and need to execute.
    timed_phase!(
        "optimize",
        crate::optimizer::optimize(&mut program, four_state, optimize_options)
    );
    timed_phase!(
        "verify_sir_after_optimize",
        verify_program_sir(&program, "after optimization")
    )?;

    if let Some(t) = trace
        && trace_opts.post_optimized_sir
    {
        t.post_optimized_sir = Some(program.clone());
    }

    Ok(program)
}

fn relocate_executation_unit_with_errors<A, B>(
    eu: &ExecutionUnit<A>,
    f: &impl Fn(&A) -> B,
    runtime_error_codes: &HashMap<i64, i64>,
    runtime_event_sites: &HashMap<u32, u32>,
) -> ExecutionUnit<B> {
    ExecutionUnit {
        entry_block_id: eu.entry_block_id,
        blocks: eu
            .blocks
            .iter()
            .map(|(id, block)| {
                (
                    *id,
                    crate::ir::BasicBlock {
                        id: block.id,
                        instructions: block
                            .instructions
                            .iter()
                            .map(|inst| match inst {
                                crate::ir::SIRInstruction::RuntimeEvent { site_id, args } => {
                                    crate::ir::SIRInstruction::RuntimeEvent {
                                        site_id: runtime_event_sites
                                            .get(site_id)
                                            .copied()
                                            .unwrap_or(*site_id),
                                        args: args.clone(),
                                    }
                                }
                                crate::ir::SIRInstruction::CombCaptureEvent {
                                    site_id,
                                    args,
                                    fatal_error_code,
                                    consume_enabled,
                                } => crate::ir::SIRInstruction::CombCaptureEvent {
                                    site_id: runtime_event_sites
                                        .get(site_id)
                                        .copied()
                                        .unwrap_or(*site_id),
                                    args: args.clone(),
                                    fatal_error_code: *fatal_error_code,
                                    consume_enabled: *consume_enabled,
                                },
                                _ => inst.map_addr(f),
                            })
                            .collect(),
                        params: block.params.clone(),
                        terminator: match block.terminator {
                            crate::ir::SIRTerminator::Error(code) => {
                                crate::ir::SIRTerminator::Error(
                                    runtime_error_codes.get(&code).copied().unwrap_or(code),
                                )
                            }
                            ref terminator => terminator.clone(),
                        },
                    },
                )
            })
            .collect(),
        register_map: eu.register_map.clone(),
    }
}

fn unify_clock_domains(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];

        // Internal aliases (e.g. `assign clk_internal = clk_port;`)
        for logic_path in &sim_module.comb_blocks {
            // Only unify direct aliases, not complex logic like gated clocks
            if logic_path.sources.len() == 1 {
                let expr_node = sim_module.arena.get(logic_path.expr);
                let is_alias = matches!(
                    expr_node,
                    crate::logic_tree::SLTNode::Input { .. }
                        | crate::logic_tree::SLTNode::Slice { .. }
                );
                if is_alias {
                    let Some(target) = logic_path.target.var() else {
                        continue;
                    };
                    let target_abs = AbsoluteAddr {
                        instance_id: *id,
                        var_id: target.id,
                    };
                    let source_abs = AbsoluteAddr {
                        instance_id: *id,
                        var_id: logic_path.sources.iter().next().unwrap().id,
                    };
                    drive_graph.entry(source_abs).or_default().push(target_abs);
                }
            }
        }
        for (inst_name, glue_blocks) in &sim_module.glue_blocks {
            for (idx, glue_block) in glue_blocks.iter().enumerate() {
                let mut child_path = path.0.clone();
                child_path.push((*inst_name, idx));
                let child_id = expanded[&InstancePath(child_path)];

                // Inputs: Parent -> Child (Parent drives Child)
                for (parent_vars, logic_path) in &glue_block.input_ports {
                    if let Some(target) = logic_path.target.var()
                        && let GlueAddr::Child(child_var_id) = target.id
                    {
                        let child_abs = AbsoluteAddr {
                            instance_id: child_id,
                            var_id: child_var_id,
                        };
                        for parent_var in parent_vars {
                            let parent_abs = AbsoluteAddr {
                                instance_id: *id,
                                var_id: *parent_var,
                            };
                            drive_graph.entry(parent_abs).or_default().push(child_abs);
                        }
                    }
                }
                // Outputs: Child -> Parent (Child drives Parent)
                for (parent_vars, logic_path) in &glue_block.output_ports {
                    for parent_var in parent_vars {
                        let parent_abs = AbsoluteAddr {
                            instance_id: *id,
                            var_id: *parent_var,
                        };
                        for source in &logic_path.sources {
                            if let GlueAddr::Child(child_var_id) = source.id {
                                let child_abs = AbsoluteAddr {
                                    instance_id: child_id,
                                    var_id: child_var_id,
                                };
                                drive_graph.entry(child_abs).or_default().push(parent_abs);
                            }
                        }
                    }
                }
            }
        }
    }

    // Resolve Canonical Clock Domains: Find the root driver for each connected component
    let mut clock_domains: HashMap<AbsoluteAddr, AbsoluteAddr> = HashMap::default();

    // Reverse the drive graph to find roots (Sink -> Sources)
    let mut reverse_drive_graph: HashMap<AbsoluteAddr, Vec<AbsoluteAddr>> = HashMap::default();
    for (src, sinks) in &drive_graph {
        for sink in sinks {
            reverse_drive_graph.entry(*sink).or_default().push(*src);
        }
    }

    // Collect all unique addresses involved in any drive relationship
    let mut all_addrs = HashSet::default();
    for src in drive_graph.keys() {
        all_addrs.insert(*src);
    }
    for sinks in drive_graph.values() {
        for sink in sinks {
            all_addrs.insert(*sink);
        }
    }

    // Assign each address its canonical root driver
    for addr in all_addrs {
        let mut current = addr;
        let mut visited = HashSet::default();
        // Traverse upwards towards the root driver
        while let Some(sources) = reverse_drive_graph.get(&current) {
            if sources.is_empty() {
                break;
            }
            // In a valid hardware design, a clock net usually has 1 driver.
            // If multiple, we just pick the first for canonicalization.
            let next = sources[0];
            if visited.contains(&next) {
                break; // Prevent infinite loop in case of bad combinational loop
            }
            visited.insert(next);
            current = next;
        }
        clock_domains.insert(addr, current);
    }
    clock_domains
}

fn relocate_units(
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    global_boundaries: &HashMap<AbsoluteAddr, std::collections::BTreeSet<usize>>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    trace_opts: &crate::debug::TraceOptions,
    trace: &mut Option<&mut crate::debug::CompilationTrace>,
) -> Result<
    (
        SLTNodeArena<AbsoluteAddr>,
        HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
        HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
        Vec<crate::logic_tree::LogicPath<AbsoluteAddr>>,
        Vec<crate::ir::CombObserver<AbsoluteAddr>>,
        HashMap<i64, RuntimeErrorInfo<AbsoluteAddr>>,
        Vec<crate::ir::RuntimeEventSite>,
        i64,
    ),
    ParserError,
> {
    let mut global_arena = SLTNodeArena::<AbsoluteAddr>::new();
    let mut eval_apply_ffs: HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>,
    > = HashMap::default();
    let mut eval_only_ffs: HashMap<
        AbsoluteAddr,
        Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>,
    > = HashMap::default();
    let mut apply_ffs: HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>> =
        HashMap::default();
    let mut comb_blocks = Vec::new();
    let mut comb_observers = Vec::new();
    let mut runtime_errors = HashMap::default();
    let mut runtime_event_sites = Vec::new();
    let mut next_runtime_error_code = 2000;

    for (path, id) in expanded {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        let mut runtime_error_codes = HashMap::default();
        for (&local_code, info) in &sim_module.runtime_errors {
            let global_code = next_runtime_error_code;
            next_runtime_error_code += 1;
            runtime_error_codes.insert(local_code, global_code);
            runtime_errors.insert(
                global_code,
                RuntimeErrorInfo {
                    message: info.message.clone(),
                    signals: info
                        .signals
                        .iter()
                        .filter(|var_id| sim_module.variables.contains_key(var_id))
                        .map(|&var_id| AbsoluteAddr {
                            instance_id: *id,
                            var_id,
                        })
                        .collect(),
                },
            );
        }
        let mut runtime_event_site_map = HashMap::default();
        for (local_site, site) in sim_module.runtime_event_sites.iter().enumerate() {
            let global_site = runtime_event_sites.len() as u32;
            runtime_event_site_map.insert(local_site as u32, global_site);
            runtime_event_sites.push(site.clone());
        }

        let arena_start = global_arena.len();
        let mut relocated_module = flatting::flatting(
            sim_module,
            path,
            expanded,
            global_boundaries,
            &mut global_arena,
            trace_opts,
            trace.as_deref_mut(),
        )?;
        remap_for_fold_runtime_event_sites(
            &mut global_arena,
            arena_start,
            &runtime_event_site_map,
        )?;
        for observer in &mut relocated_module.comb_observers {
            observer.site_id = runtime_event_site_map[&observer.site_id];
            observer.activation_group = runtime_event_site_map[&observer.activation_group];
        }
        comb_blocks.extend(relocated_module.comb_blocks);
        comb_observers.extend(relocated_module.comb_observers);

        // Relocate sequential blocks for this instance
        for (trigger_set, eu) in &sim_module.eval_apply_ff_blocks {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);

            eval_apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    },
                    &runtime_error_codes,
                    &runtime_event_site_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        },
                        &runtime_error_codes,
                        &runtime_event_site_map,
                    ),
                );
            }
        }

        for (trigger_set, eu) in &sim_module.eval_only_ff_blocks {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            eval_only_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    },
                    &runtime_error_codes,
                    &runtime_event_site_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                eval_only_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        },
                        &runtime_error_codes,
                        &runtime_event_site_map,
                    ),
                );
            }
        }

        for (trigger_set, eu) in &sim_module.apply_ff_blocks {
            let clock_addr = AbsoluteAddr {
                instance_id: *id,
                var_id: trigger_set.clock,
            };
            let canonical_addr = clock_domains
                .get(&clock_addr)
                .copied()
                .unwrap_or(clock_addr);
            apply_ffs.entry(canonical_addr).or_default().push(
                relocate_executation_unit_with_errors(
                    eu,
                    &|addr| RegionedAbsoluteAddr {
                        region: addr.region,
                        instance_id: *id,
                        var_id: addr.var_id,
                    },
                    &runtime_error_codes,
                    &runtime_event_site_map,
                ),
            );

            for &reset in &trigger_set.resets {
                let reset_addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: reset,
                };
                let canonical_addr = clock_domains
                    .get(&reset_addr)
                    .copied()
                    .unwrap_or(reset_addr);
                apply_ffs.entry(canonical_addr).or_default().push(
                    relocate_executation_unit_with_errors(
                        eu,
                        &|addr| RegionedAbsoluteAddr {
                            region: addr.region,
                            instance_id: *id,
                            var_id: addr.var_id,
                        },
                        &runtime_error_codes,
                        &runtime_event_site_map,
                    ),
                );
            }
        }
    }
    Ok((
        global_arena,
        eval_apply_ffs,
        eval_only_ffs,
        apply_ffs,
        comb_blocks,
        comb_observers,
        runtime_errors,
        runtime_event_sites,
        next_runtime_error_code,
    ))
}

fn build_comb_observer_capture_paths(
    comb_blocks: &mut Vec<LogicPath<AbsoluteAddr>>,
    observers: &mut [crate::ir::CombObserver<AbsoluteAddr>],
    sites: &[crate::ir::RuntimeEventSite],
    arena: &mut SLTNodeArena<AbsoluteAddr>,
) -> Result<(), ParserError> {
    if observers.is_empty() {
        return Ok(());
    }

    annotate_comb_capture_enable_sites(comb_blocks, observers);

    let mut group_members: HashMap<u32, Vec<usize>> = HashMap::default();
    for (idx, observer) in observers.iter().enumerate() {
        group_members
            .entry(observer.activation_group)
            .or_default()
            .push(idx);
    }
    let mut emitted_group_triggers = HashSet::default();
    let mut previous_primary_capture_path: Option<LogicPathId> = None;
    let mut previous_trigger_capture_path: Option<LogicPathId> = None;
    for observer_idx in 0..observers.len() {
        let observer = &observers[observer_idx];
        let has_statement_position_dependency =
            observer_has_statement_position_dependency(comb_blocks, observer);
        let order_before = observer_order_before(comb_blocks, observer);
        let order_after = observer_order_after(comb_blocks, observer);
        let trigger_paths = if has_statement_position_dependency {
            observer_trigger_paths(comb_blocks, observer)
        } else {
            Vec::new()
        };
        if observer.captured_in_loop {
            let Some(loop_runner) = observer.loop_runner else {
                continue;
            };
            let sources: HashSet<_> = observer
                .sensitivity
                .iter()
                .copied()
                .filter(|atom| !observer_written_input_overlaps(observer, atom))
                .filter(|atom| !observer_statement_position_overlaps(comb_blocks, observer, atom))
                .collect();
            let path_id = LogicPathId(comb_blocks.len());
            if let Some(prev) = previous_primary_capture_path {
                comb_blocks[prev.0].order_before.insert(path_id);
            }
            for idx in &order_after {
                comb_blocks[idx.0].order_before.insert(path_id);
            }
            comb_blocks.push(LogicPath {
                target: LogicPathTarget::CombCaptureEvent {
                    site_id: observer.site_id,
                    guard: None,
                    emit_on_true: true,
                    args: Vec::new(),
                    loop_runner: Some(loop_runner),
                    fatal_error_code: None,
                    consume_enabled: true,
                },
                sources,
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: observer.local_inputs.clone(),
                order_before: order_before.clone(),
                comb_capture_enable_sites: Vec::new(),
                pre_lower_nodes: Vec::new(),
                expr: loop_runner,
            });
            previous_primary_capture_path = Some(path_id);
            for trigger_idx in trigger_paths {
                let Some(trigger_target) = comb_blocks[trigger_idx.0].target.var().copied() else {
                    continue;
                };
                let path_id = LogicPathId(comb_blocks.len());
                if let Some(prev) = previous_trigger_capture_path {
                    comb_blocks[prev.0].order_before.insert(path_id);
                }
                comb_blocks[trigger_idx.0].order_before.insert(path_id);
                comb_blocks.push(LogicPath {
                    target: LogicPathTarget::CombCaptureEvent {
                        site_id: observer.site_id,
                        guard: None,
                        emit_on_true: true,
                        args: Vec::new(),
                        loop_runner: Some(loop_runner),
                        fatal_error_code: None,
                        consume_enabled: true,
                    },
                    sources: std::iter::once(trigger_target).collect(),
                    previous_sources: HashSet::default(),
                    address_sources: HashSet::default(),
                    local_inputs: observer.local_inputs.clone(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    pre_lower_nodes: Vec::new(),
                    expr: loop_runner,
                });
                previous_trigger_capture_path = Some(path_id);
            }
            continue;
        }
        let local_input_ids: HashSet<_> = observer
            .local_inputs
            .iter()
            .map(|(addr, _)| *addr)
            .collect();
        let mut sources: HashSet<_> = observer
            .observed_inputs
            .iter()
            .copied()
            .filter(|atom| !observer_written_input_overlaps(observer, atom))
            .filter(|atom| !local_input_ids.contains(&atom.id))
            .filter(|atom| !observer_statement_position_overlaps(comb_blocks, observer, atom))
            .collect();
        for (_, node) in &observer.local_inputs {
            let mut local_sources = HashSet::default();
            crate::flatting::collect_inputs(*node, arena, &mut local_sources);
            sources.extend(
                local_sources
                    .into_iter()
                    .filter(|atom| !observer_written_input_overlaps(observer, atom))
                    .filter(|atom| !local_input_ids.contains(&atom.id))
                    .filter(|atom| {
                        !observer_statement_position_overlaps(comb_blocks, observer, atom)
                    }),
            );
        }
        let expr = match observer.guard.or_else(|| observer.args.first().copied()) {
            Some(expr) => expr,
            None => arena.alloc(crate::logic_tree::SLTNode::Constant(
                num_bigint::BigUint::from(1u8),
                num_bigint::BigUint::from(0u8),
                1,
                false,
            ))?,
        };
        let emit_on_true = matches!(
            sites[observer.site_id as usize].kind,
            crate::ir::RuntimeEventKind::Display
        );
        let fatal_error_code = matches!(
            sites[observer.site_id as usize].kind,
            crate::ir::RuntimeEventKind::AssertFatal
        )
        .then_some(observer.site_id as i64);
        let pre_lower_nodes = observer_pre_lower_nodes(observer);
        for idx in &order_after {
            comb_blocks[idx.0]
                .pre_lower_nodes
                .extend(pre_lower_nodes.iter().copied());
        }
        let path_id = LogicPathId(comb_blocks.len());
        if let Some(prev) = previous_primary_capture_path {
            comb_blocks[prev.0].order_before.insert(path_id);
        }
        for idx in &order_after {
            comb_blocks[idx.0].order_before.insert(path_id);
        }
        comb_blocks.push(LogicPath {
            target: LogicPathTarget::CombCaptureEvent {
                site_id: observer.site_id,
                guard: observer.guard,
                emit_on_true,
                args: observer.args.clone(),
                loop_runner: None,
                fatal_error_code,
                consume_enabled: !trigger_paths.is_empty(),
            },
            sources,
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: observer.local_inputs.clone(),
            order_before,
            comb_capture_enable_sites: Vec::new(),
            pre_lower_nodes: Vec::new(),
            expr,
        });
        previous_primary_capture_path = Some(path_id);
        for trigger_idx in trigger_paths {
            if !emitted_group_triggers.insert((observer.activation_group, trigger_idx)) {
                continue;
            }
            let Some(trigger_target) = comb_blocks[trigger_idx.0].target.var().copied() else {
                continue;
            };
            for &member_idx in &group_members[&observer.activation_group] {
                let member = &observers[member_idx];
                let member_emit_on_true = matches!(
                    sites[member.site_id as usize].kind,
                    crate::ir::RuntimeEventKind::Display
                );
                let member_fatal_error_code = matches!(
                    sites[member.site_id as usize].kind,
                    crate::ir::RuntimeEventKind::AssertFatal
                )
                .then_some(member.site_id as i64);
                let member_expr = match member
                    .loop_runner
                    .or(member.guard)
                    .or_else(|| member.args.first().copied())
                {
                    Some(expr) => expr,
                    None => arena.alloc(crate::logic_tree::SLTNode::Constant(
                        num_bigint::BigUint::from(1u8),
                        num_bigint::BigUint::from(0u8),
                        1,
                        false,
                    ))?,
                };
                let path_id = LogicPathId(comb_blocks.len());
                if let Some(prev) = previous_trigger_capture_path {
                    comb_blocks[prev.0].order_before.insert(path_id);
                }
                comb_blocks[trigger_idx.0].order_before.insert(path_id);
                comb_blocks.push(LogicPath {
                    target: LogicPathTarget::CombCaptureEvent {
                        site_id: member.site_id,
                        guard: member.guard,
                        emit_on_true: member_emit_on_true,
                        args: member.args.clone(),
                        loop_runner: member.loop_runner,
                        fatal_error_code: member_fatal_error_code,
                        consume_enabled: true,
                    },
                    sources: std::iter::once(trigger_target).collect(),
                    previous_sources: HashSet::default(),
                    address_sources: HashSet::default(),
                    local_inputs: member.local_inputs.clone(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    pre_lower_nodes: Vec::new(),
                    expr: member_expr,
                });
                previous_trigger_capture_path = Some(path_id);
            }
        }
    }
    Ok(())
}

fn apply_always_comb_previous_source_ordering(comb_blocks: &mut [LogicPath<AbsoluteAddr>]) {
    let targets: Vec<_> = comb_blocks
        .iter()
        .map(|path| path.target.var().copied())
        .collect();

    for (idx, path) in comb_blocks.iter_mut().enumerate() {
        if path.previous_sources.is_empty() {
            continue;
        }

        let previous_sources = path.previous_sources.clone();
        let address_sources = path.address_sources.clone();
        path.sources.retain(|source| {
            let is_previous = previous_sources.iter().any(|previous| {
                previous.id == source.id && previous.access.overlaps(&source.access)
            });
            let is_address = address_sources
                .iter()
                .any(|address| address.id == source.id && address.access.overlaps(&source.access));
            !is_previous || is_address
        });

        let mut order_before = Vec::new();
        for (target_idx, target) in targets.iter().enumerate() {
            if target_idx == idx {
                continue;
            }
            let Some(target) = target else {
                continue;
            };
            if previous_sources.iter().any(|previous| {
                previous.id == target.id && previous.access.overlaps(&target.access)
            }) {
                order_before.push(LogicPathId(target_idx));
            }
        }
        path.order_before.extend(order_before);
    }
}

fn observer_written_input_overlaps(
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    observer
        .written_input_atoms
        .iter()
        .any(|written| written.id == atom.id && written.access.overlaps(&atom.access))
}

fn observer_statement_position_overlaps(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
    atom: &VarAtomBase<AbsoluteAddr>,
) -> bool {
    atom_overlaps_any(atom, observer_affected_by_preceding_writes(paths, observer))
}

fn observer_has_statement_position_dependency(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
) -> bool {
    if observer.preceding_writes.is_empty() {
        return false;
    }
    let affected = observer_affected_by_preceding_writes(paths, observer);
    observer
        .position_inputs
        .iter()
        .chain(observer.observed_inputs.iter())
        .any(|input| atom_overlaps_any(input, &affected))
}

fn observer_trigger_paths(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
) -> Vec<LogicPathId> {
    let mut seen_targets = HashSet::default();
    let affected = observer_affected_by_preceding_writes(paths, observer);
    paths
        .iter()
        .enumerate()
        .filter_map(|(idx, path)| {
            let target = path.target.var()?;
            if observer_written_input_overlaps(observer, target) {
                return None;
            }
            let matches_observer_operand = observer
                .position_inputs
                .iter()
                .chain(observer.observed_inputs.iter())
                .any(|atom| target.id == atom.id && target.access.overlaps(&atom.access));
            if !matches_observer_operand
                || !atom_overlaps_any(target, &affected)
                || !seen_targets.insert((target.id, target.access.lsb, target.access.msb))
            {
                return None;
            }
            Some(LogicPathId(idx))
        })
        .collect()
}

fn observer_pre_lower_nodes(observer: &crate::ir::CombObserver<AbsoluteAddr>) -> Vec<NodeId> {
    if !observer.local_inputs.is_empty() {
        return Vec::new();
    }
    let mut nodes = Vec::with_capacity(observer.args.len() + usize::from(observer.guard.is_some()));
    if let Some(guard) = observer.guard {
        nodes.push(guard);
    }
    nodes.extend(observer.args.iter().copied());
    nodes
}

fn annotate_comb_capture_enable_sites(
    comb_blocks: &mut [LogicPath<AbsoluteAddr>],
    observers: &[crate::ir::CombObserver<AbsoluteAddr>],
) {
    let mut group_sites: HashMap<u32, Vec<u32>> = HashMap::default();
    for observer in observers {
        group_sites
            .entry(observer.activation_group)
            .or_default()
            .push(observer.site_id);
    }
    for observer in observers {
        for atom in &observer.sensitivity {
            for path in comb_blocks.iter_mut() {
                let Some(target) = path.target.var() else {
                    continue;
                };
                if target.id == atom.id && target.access.overlaps(&atom.access) {
                    for site_id in &group_sites[&observer.activation_group] {
                        if !path.comb_capture_enable_sites.contains(site_id) {
                            path.comb_capture_enable_sites.push(*site_id);
                        }
                    }
                }
            }
        }
    }
}

fn observer_order_after(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
) -> HashSet<LogicPathId> {
    let mut result = HashSet::default();
    if !observer_has_statement_position_dependency(paths, observer) {
        return result;
    }
    for written in &observer.preceding_writes {
        for (idx, path) in paths.iter().enumerate() {
            let Some(target) = path.target.var() else {
                continue;
            };
            if target.id == written.id && target.access.overlaps(&written.access) {
                result.insert(LogicPathId(idx));
            }
        }
    }
    result
}

fn observer_order_before(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
) -> HashSet<LogicPathId> {
    if !observer_has_statement_position_dependency(paths, observer) {
        return HashSet::default();
    }
    let preceding_writes = observer.preceding_writes.iter().collect::<Vec<_>>();
    let affected_by_preceding_writes = observer_affected_by_preceding_writes(paths, observer);
    let mut result = HashSet::default();
    for (idx, path) in paths.iter().enumerate() {
        let Some(target) = path.target.var() else {
            continue;
        };
        if !atom_overlaps_any(target, &affected_by_preceding_writes) {
            continue;
        }
        let already_written = preceding_writes
            .iter()
            .any(|written| target.id == written.id && target.access.overlaps(&written.access));
        if !already_written {
            result.insert(LogicPathId(idx));
        }
    }
    result
}

fn observer_affected_by_preceding_writes(
    paths: &[LogicPath<AbsoluteAddr>],
    observer: &crate::ir::CombObserver<AbsoluteAddr>,
) -> HashSet<VarAtomBase<AbsoluteAddr>> {
    let mut affected: HashSet<VarAtomBase<AbsoluteAddr>> =
        observer.preceding_writes.iter().copied().collect();
    let mut changed = true;
    while changed {
        changed = false;
        for path in paths {
            let Some(target) = path.target.var() else {
                continue;
            };
            if !path
                .sources
                .iter()
                .any(|source| atom_overlaps_any(source, &affected))
            {
                continue;
            }
            if affected.insert(*target) {
                changed = true;
            }
        }
    }
    affected
}

fn atom_overlaps_any<A: Eq + std::hash::Hash + Copy>(
    atom: &VarAtomBase<A>,
    atoms: impl IntoIterator<Item = impl std::borrow::Borrow<VarAtomBase<A>>>,
) -> bool {
    atoms.into_iter().any(|other| {
        let other = other.borrow();
        atom.id == other.id && atom.access.overlaps(&other.access)
    })
}

fn analyze_clock_dependencies(
    eval_apply_ffs: &mut HashMap<AbsoluteAddr, Vec<crate::ir::ExecutionUnit<RegionedAbsoluteAddr>>>,
    comb_blocks: &[LogicPath<AbsoluteAddr>],
    arena: &SLTNodeArena<AbsoluteAddr>,
    clock_domains: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    expanded: &HashMap<InstancePath, InstanceId>,
    instance_modules: &HashMap<InstanceId, ModuleId>,
    modules: &HashMap<ModuleId, SimModule>,
    config: &BuildConfig,
) -> (Vec<AbsoluteAddr>, BTreeSet<AbsoluteAddr>) {
    // Build static clock dependency graph & Topo Sort
    let mut clock_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    let mut unique_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();

    // 1. Identify all variables written by FFs (direct sequential outputs)
    let mut ff_outputs: BTreeSet<AbsoluteAddr> = BTreeSet::new();

    for (domain_clock, eus) in &*eval_apply_ffs {
        unique_clocks.insert(*domain_clock);
        for eu in eus {
            for bb in eu.blocks.values() {
                for inst in &bb.instructions {
                    if let crate::ir::SIRInstruction::Store(target_addr, ..) = inst {
                        // Direct sequential dependency: the target is driven by this clock
                        let abs = target_addr.absolute_addr();
                        let canonical_target = clock_domains.get(&abs).copied().unwrap_or(abs);

                        ff_outputs.insert(abs);

                        if canonical_target != *domain_clock {
                            clock_deps
                                .entry(canonical_target)
                                .or_default()
                                .insert(*domain_clock);
                        }
                    }
                }
            }
        }
    }

    // 2. Build combinational dependency graph (target -> sources)
    let acd_timing = std::env::var("CELOX_PHASE_TIMING").is_ok();
    let acd_start = acd_timing.then(crate::timing::now);
    let mut comb_deps: BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>> = BTreeMap::new();
    for path in comb_blocks {
        let Some(target) = path.target.var() else {
            continue;
        };
        let target_abs = target.id;
        let mut sources = crate::HashSet::default();
        crate::flatting::collect_inputs(path.expr, arena, &mut sources);
        for source in sources {
            comb_deps.entry(target_abs).or_default().insert(source.id);
        }
    }
    if let Some(s) = acd_start {
        eprintln!(
            "[acd] comb_deps build ({} blocks): {:?}",
            comb_deps.len(),
            s.elapsed()
        );
    }

    // 3. Propagate FF outputs through combinational graph to find all derived variables
    let fp_start = acd_timing.then(crate::timing::now);
    let mut derived_from_ff: BTreeSet<AbsoluteAddr> = ff_outputs.clone();
    let mut changed = true;
    let mut fp_rounds = 0u32;
    while changed {
        changed = false;
        fp_rounds += 1;
        for (target, sources) in &comb_deps {
            if !derived_from_ff.contains(target) {
                // If any source is derived from an FF, the target is too
                if sources.iter().any(|s| derived_from_ff.contains(s)) {
                    derived_from_ff.insert(*target);
                    changed = true;
                }
            }
        }
    }
    if let Some(s) = fp_start {
        eprintln!(
            "[acd] fixpoint: {fp_rounds} rounds, {} entries, {:?}",
            comb_deps.len(),
            s.elapsed()
        );
    }

    // 4. Any clock domain that is derived from an FF is a cascaded clock!
    // We add them to a special "pseudo-domain" or just add themselves to trigger cascade marking.
    for clk in &unique_clocks {
        if derived_from_ff.contains(clk) {
            // Self-dependency ensures it appears in `clock_deps.keys()`
            clock_deps.entry(*clk).or_default().insert(*clk);
        }
    }

    // Topologically sort the clock domains
    // Sources (no dependencies) should be evaluated first.
    let mut topological_clocks = Vec::new();
    let mut visited = BTreeSet::new();
    let mut temp_visited = BTreeSet::new();

    fn topo_visit(
        node: AbsoluteAddr,
        deps: &BTreeMap<AbsoluteAddr, BTreeSet<AbsoluteAddr>>,
        visited: &mut BTreeSet<AbsoluteAddr>,
        temp_visited: &mut BTreeSet<AbsoluteAddr>,
        result: &mut Vec<AbsoluteAddr>,
    ) {
        if visited.contains(&node) {
            return;
        }
        if temp_visited.contains(&node) {
            // Cycle detected in clock generation, ignore and break cycle for now
            return;
        }
        temp_visited.insert(node);

        if let Some(node_deps) = deps.get(&node) {
            for &dep in node_deps {
                topo_visit(dep, deps, visited, temp_visited, result);
            }
        }

        temp_visited.remove(&node);
        visited.insert(node);
        result.push(node);
    }

    // Ensure all unique clocks mapped in eval_apply_ffs are present in the topo sort
    for &clk in &unique_clocks {
        if !visited.contains(&clk) {
            topo_visit(
                clk,
                &clock_deps,
                &mut visited,
                &mut temp_visited,
                &mut topological_clocks,
            );
        }
    }

    // Include other potential event signals (like synchronous resets) so they can be scheduled
    for id in expanded.values() {
        let module_id = &instance_modules[id];
        let sim_module = &modules[module_id];
        for (var_id, var) in &sim_module.variables {
            let kind = type_kind_to_domain_kind(&var.r#type.kind, config);
            let is_trigger = matches!(
                kind,
                DomainKind::ClockPosedge
                    | DomainKind::ClockNegedge
                    | DomainKind::ResetAsyncHigh
                    | DomainKind::ResetAsyncLow
            );
            if is_trigger {
                let addr = AbsoluteAddr {
                    instance_id: *id,
                    var_id: *var_id,
                };
                let canonical = clock_domains.get(&addr).copied().unwrap_or(addr);
                // Add empty execution units so it becomes a valid event domain for scheduling
                eval_apply_ffs.entry(canonical).or_default();

                if !visited.contains(&canonical) {
                    topo_visit(
                        canonical,
                        &clock_deps,
                        &mut visited,
                        &mut temp_visited,
                        &mut topological_clocks,
                    );
                }
            }
        }
    }

    let mut cascaded_clocks: BTreeSet<AbsoluteAddr> = BTreeSet::new();
    for (target, sources) in &clock_deps {
        cascaded_clocks.insert(*target);
        for source in sources {
            cascaded_clocks.insert(*source);
        }
    }

    (topological_clocks, cascaded_clocks)
}
