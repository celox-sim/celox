use std::path::Path;
use std::sync::Arc;

use celox_sv_analyzer::ir::SourceSpan;

/// A non-fatal diagnostic of the SystemVerilog frontend. The design still
/// compiles; the simulator reports these with its compilation warnings.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{kind}")]
pub struct FrontendWarning {
    pub kind: FrontendWarningKind,
    /// The construct that warns, unless the analyzer created it or its file
    /// cannot be read.
    pub location: Option<WarningLocation>,
}

/// Located warnings identify the warning category and source call, independent
/// of the parameter-dependent payload produced by an individual specialization.
impl PartialEq for FrontendWarning {
    fn eq(&self, other: &Self) -> bool {
        match (&self.location, &other.location) {
            (Some(left), Some(right)) => {
                std::mem::discriminant(&self.kind) == std::mem::discriminant(&other.kind)
                    && (&left.path, left.line, left.column)
                        == (&right.path, right.line, right.column)
            }
            (None, None) => self.kind == other.kind,
            _ => false,
        }
    }
}

impl Eq for FrontendWarning {}

/// What a [`FrontendWarningKind::MixedTimescale`] element lacks.
fn defaulted_parts(unit: bool, precision: bool) -> &'static str {
    match (unit, precision) {
        (true, true) => "unit or precision",
        (true, false) => "unit",
        _ => "precision",
    }
}

/// What a [`FrontendWarning`] reports.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, thiserror::Error)]
pub enum FrontendWarningKind {
    #[error("latch inference inside always_comb: assignments retain their previous values")]
    InferredLatch,
    /// Design element `element` takes the default time unit (`unit`) or
    /// precision (`precision`) while others declare theirs. IEEE 1800-2023
    /// 3.14.2.3 makes that an error; relaxed, the defaults apply.
    #[error(
        "design element `{element}` declares no time {} while others do; it takes the default",
        defaulted_parts(*.unit, *.precision)
    )]
    MixedTimescale {
        element: String,
        unit: bool,
        precision: bool,
    },
    /// A design declares a module more than once and uses the first.
    #[error("module `{name}` is declared more than once; the first declaration is used")]
    DuplicateModule { name: String },
    #[error("`$size` on an interface array is a language extension")]
    InterfaceArrayQuery,
    #[error(
        "unindexed interface array parameter `{path}` uses the first declared element `{element}`"
    )]
    InterfaceArrayParameter { path: String, element: String },
    /// IEEE 1800-2023 9.2.2.4 restricts writes by other processes.
    #[error("multiple variable drivers for `{signal}` in `{module}` include an always_ff process")]
    MultipleAlwaysFfDrivers { module: String, signal: String },
    /// Aggregate defaults are a compatibility extension to IEEE 1800-2023 23.2.2.4.
    #[error("default value on unpacked input port `{module}.{port}` is a language extension")]
    UnpackedPortDefault { module: String, port: String },
    /// A specialization whose variables or nets `force` statements force
    /// through hierarchical names: every instance of it carries the force
    /// controls (IEEE 1800-2023 10.6.2).
    #[error(
        "the force of {forced} in an instance of `{module}` gives all {instances} instances of the specialization force controls"
    )]
    ForcedSpecialization {
        module: String,
        forced: String,
        instances: usize,
    },
    /// An unpacked array that `force` statements force elements of: its
    /// force controls are arrays of its shape (IEEE 1800-2023 10.6.2).
    #[error(
        "the force of elements of `{array}` in `{module}` gives it force controls of {bits} bits"
    )]
    ForcedArray {
        module: String,
        array: String,
        bits: usize,
    },
    /// `$exit` outside a thread of a program's `initial` procedure, which
    /// IEEE 1800-2023 24.7 ignores.
    #[error("`$exit` outside a program block is ignored")]
    ExitOutsideProgram,
    /// A variable that only several `always @*` blocks write is a temporary
    /// of each block, so its writes do not wake the other blocks as IEEE
    /// 1800-2023 9.4.2.2 requires. Applied only when the diagnostic is not
    /// an error.
    #[error(
        "`{signal}` in `{module}` is treated as a temporary of each `always @*` block that writes it ({blocks}): its writes do not wake the other blocks"
    )]
    SharedAlwaysTemporary {
        module: String,
        signal: String,
        blocks: String,
    },
    /// A static variable whose declaration specifies an initializer without
    /// the `static` keyword (IEEE 1800-2023 6.21).
    #[error("static variable `{name}` has an initializer but is not declared `static`")]
    ImplicitStaticInitializer { name: String },
    #[error(
        "assigning a packed value or unbased fill directly to an unpacked array is a language extension"
    )]
    PackedToUnpackedAssignment,
    #[error(
        "force or release of a variable select, variable member, or unpacked array element is a language extension"
    )]
    NonstandardForceTarget,
    #[error("`$psprintf` is a vendor alias for the standard system function `$sformatf`")]
    PsprintfAlias,
    /// A call of `$set_coverage_db_name` or `$load_coverage_db` (`task`):
    /// there is no coverage database, so nothing is saved or loaded (IEEE
    /// 1800-2023 19.9).
    #[error("`{task}` has no effect: Celox keeps no coverage database")]
    NoCoverageDatabase { task: &'static str },
    /// A value or a range of a bin of a coverpoint that takes no part in the
    /// bin, or only a part of which does (IEEE 1800-2023 19.5.7).
    #[error("bin `{bin}` of coverpoint `{item}` of covergroup `{covergroup}`: {detail}")]
    CovergroupBinValue {
        covergroup: String,
        item: String,
        bin: String,
        detail: String,
    },
}

/// Where a warning is in the source.
#[derive(Debug, Clone)]
pub struct WarningLocation {
    pub path: String,
    /// The 1-based line of the start of the construct.
    pub line: usize,
    /// The 1-based column, in characters, of the start of the construct.
    pub column: usize,
    source: miette::NamedSource<Arc<str>>,
    span: miette::SourceSpan,
}

impl WarningLocation {
    fn new(path: String, text: Arc<str>, span: &SourceSpan) -> Option<Self> {
        let before = text.get(..span.start)?;
        text.get(span.start..span.end)?;
        let line_start = before.rfind('\n').map_or(0, |newline| newline + 1);
        Some(Self {
            line: before.matches('\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
            source: miette::NamedSource::new(&path, text),
            span: (span.start..span.end).into(),
            path,
        })
    }
}

/// A warning found while lowering, with the span of its construct.
pub(crate) type PendingWarning = (FrontendWarningKind, Option<SourceSpan>);

/// The warnings of an analysis, as warnings of the frontend.
pub(crate) fn analyzer_warnings(
    found: &[celox_sv_analyzer::ir::AnalyzerWarning],
) -> impl Iterator<Item = PendingWarning> + '_ {
    found.iter().map(|warning| match warning {
        celox_sv_analyzer::ir::AnalyzerWarning::InterfaceArrayQuery { site } => {
            (FrontendWarningKind::InterfaceArrayQuery, site.clone())
        }
        celox_sv_analyzer::ir::AnalyzerWarning::InterfaceArrayParameter {
            path,
            element,
            site,
        } => (
            FrontendWarningKind::InterfaceArrayParameter {
                path: path.clone(),
                element: element.clone(),
            },
            site.clone(),
        ),
        celox_sv_analyzer::ir::AnalyzerWarning::MixedTimescale {
            element,
            unit,
            precision,
        } => (
            FrontendWarningKind::MixedTimescale {
                element: element.clone(),
                unit: *unit,
                precision: *precision,
            },
            None,
        ),
        celox_sv_analyzer::ir::AnalyzerWarning::DuplicateModule { name } => (
            FrontendWarningKind::DuplicateModule { name: name.clone() },
            None,
        ),
        celox_sv_analyzer::ir::AnalyzerWarning::ExitOutsideProgram { site } => {
            (FrontendWarningKind::ExitOutsideProgram, site.clone())
        }
        celox_sv_analyzer::ir::AnalyzerWarning::ImplicitStaticInitializer { name, site } => (
            FrontendWarningKind::ImplicitStaticInitializer { name: name.clone() },
            site.clone(),
        ),
        celox_sv_analyzer::ir::AnalyzerWarning::PackedToUnpackedAssignment { site } => (
            FrontendWarningKind::PackedToUnpackedAssignment,
            site.clone(),
        ),
        celox_sv_analyzer::ir::AnalyzerWarning::NonstandardForceTarget { site } => {
            (FrontendWarningKind::NonstandardForceTarget, site.clone())
        }
        celox_sv_analyzer::ir::AnalyzerWarning::PsprintfAlias { site } => {
            (FrontendWarningKind::PsprintfAlias, site.clone())
        }
        celox_sv_analyzer::ir::AnalyzerWarning::NoCoverageDatabase { task, site } => (
            FrontendWarningKind::NoCoverageDatabase { task },
            site.clone(),
        ),
        celox_sv_analyzer::ir::AnalyzerWarning::CovergroupBinValue {
            covergroup,
            item,
            bin,
            detail,
            site,
        } => (
            FrontendWarningKind::CovergroupBinValue {
                covergroup: covergroup.clone(),
                item: item.clone(),
                bin: bin.clone(),
                detail: detail.clone(),
            },
            site.clone(),
        ),
    })
}

/// Appends `found` to `warnings` in source order, once for each construct:
/// each specialization of a module and each lowering attempt of a process
/// repeats the warnings of the same source. Spans are resolved against
/// `sources`, or else the file at their path.
pub(crate) fn append_unique_warnings(
    sources: &[(&str, &Path)],
    warnings: &mut Vec<FrontendWarning>,
    mut found: Vec<PendingWarning>,
) {
    found.sort_by(|(left_kind, left), (right_kind, right)| {
        (left, left_kind).cmp(&(right, right_kind))
    });
    found.dedup_by(|(left_kind, left), (right_kind, right)| {
        match (left.as_ref(), right.as_ref()) {
            (Some(left), Some(right)) => {
                left == right
                    && std::mem::discriminant(left_kind) == std::mem::discriminant(right_kind)
            }
            _ => left == right && left_kind == right_kind,
        }
    });
    let mut texts = fxhash::FxHashMap::<std::path::PathBuf, Option<Arc<str>>>::default();
    for (kind, span) in found {
        let location = span.and_then(|span| {
            let text = texts
                .entry(span.path.clone())
                .or_insert_with(|| {
                    sources
                        .iter()
                        .find(|(_, path)| *path == span.path)
                        .map(|(code, _)| Arc::from(*code))
                        .or_else(|| std::fs::read_to_string(&span.path).ok().map(Arc::from))
                })
                .clone();
            WarningLocation::new(span.path.display().to_string(), text?, &span)
        });
        warnings.push(FrontendWarning { kind, location });
    }
}

impl miette::Diagnostic for FrontendWarning {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(match self.kind {
            FrontendWarningKind::MixedTimescale { .. } => "mixed_timescale",
            FrontendWarningKind::DuplicateModule { .. } => "duplicate_module",
            FrontendWarningKind::InterfaceArrayQuery => "interface_array_query",
            FrontendWarningKind::InterfaceArrayParameter { .. } => "interface_array_parameter",
            FrontendWarningKind::MultipleAlwaysFfDrivers { .. } => "multiple_always_ff_drivers",
            FrontendWarningKind::UnpackedPortDefault { .. } => "unpacked_port_default",
            FrontendWarningKind::InferredLatch => "inferred_latch",
            FrontendWarningKind::ExitOutsideProgram => "exit_outside_program",
            FrontendWarningKind::SharedAlwaysTemporary { .. } => "shared_always_temporary",
            FrontendWarningKind::ImplicitStaticInitializer { .. } => "implicit_static_initializer",
            FrontendWarningKind::PackedToUnpackedAssignment => "packed_to_unpacked_assignment",
            FrontendWarningKind::NonstandardForceTarget => "nonstandard_force_target",
            FrontendWarningKind::PsprintfAlias => "psprintf_alias",
            FrontendWarningKind::ForcedSpecialization { .. } => "forced_specialization",
            FrontendWarningKind::ForcedArray { .. } => "forced_array",
            FrontendWarningKind::NoCoverageDatabase { .. } => "no_coverage_database",
            FrontendWarningKind::CovergroupBinValue { .. } => "covergroup_bin_value",
        }))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Warning)
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(match self.kind {
            FrontendWarningKind::InferredLatch => {
                "assign every output on every path, or configure diagnostics.inferred_latch as warn or allow"
            }
            FrontendWarningKind::InterfaceArrayQuery => {
                "interface arrays are not data arrays in IEEE 1800-2023 20.7; set interface_array_query=warn or allow for compatibility"
            }
            FrontendWarningKind::InterfaceArrayParameter { .. } => {
                "IEEE 1800-2023 23.6 requires an instance select before a member; set interface_array_parameter=warn or allow to use the first declared element"
            }
            FrontendWarningKind::MixedTimescale { .. } => {
                "IEEE 1800-2023 3.14.2.3 makes this an error, relaxed by mixed_timescale=warn or allow; declare timeunit/timeprecision (or `timescale) in every design element to keep the standard's semantics"
            }
            FrontendWarningKind::DuplicateModule { .. } => {
                "a language deviation enabled by duplicate_module=warn or allow; remove the other declarations to keep IEEE 1800-2023 semantics"
            }
            FrontendWarningKind::MultipleAlwaysFfDrivers { .. } => {
                "IEEE 1800-2023 9.2.2.4 prohibits overlapping writers; use ordinary always procedures, or multiple_always_ff_drivers=warn or allow for compatibility"
            }
            FrontendWarningKind::UnpackedPortDefault { .. } => {
                "IEEE 1800-2023 23.2.2.4 permits defaults on singular input ports; use unpacked_port_default=warn or allow to enable aggregate defaults"
            }
            FrontendWarningKind::ExitOutsideProgram => {
                "end the simulation with `$finish`, or remove the call"
            }
            FrontendWarningKind::ForcedSpecialization { .. } => {
                "the instances no force names never set the controls; their writes only check them"
            }
            FrontendWarningKind::ForcedArray { .. } => {
                "every element of the array has an enable, a value and possibly a hold control; every write of it checks them"
            }
            FrontendWarningKind::SharedAlwaysTemporary { .. } => {
                "a language deviation enabled by the shared_always_temporary compatibility opt-in; declare the variable inside each block to keep IEEE 1800-2023 semantics"
            }
            FrontendWarningKind::ImplicitStaticInitializer { .. } => {
                "IEEE 1800-2023 6.21 requires an explicit `static` when a declaration specifies an initialization value; declare the variable `static`, or silence with implicit_static_initializer=allow"
            }
            FrontendWarningKind::PackedToUnpackedAssignment => {
                "IEEE 1800-2023 7.6 requires an explicit cast to the array type for a packed value; use an assignment pattern such as `'{default: '0}` to fill the array"
            }
            FrontendWarningKind::NonstandardForceTarget => {
                "IEEE 1800-2023 10.6.2 permits whole singular variables, nets, and constant bit- or part-selects of vector nets"
            }
            FrontendWarningKind::PsprintfAlias => {
                "use `$sformatf` as defined by IEEE 1800-2023 21.3.3"
            }
            FrontendWarningKind::NoCoverageDatabase { .. } => {
                "the argument is evaluated and nothing is saved or loaded (IEEE 1800-2023 19.9; tracking issue #4294); read the coverage of the covergroups from the simulation instead"
            }
            FrontendWarningKind::CovergroupBinValue { .. } => {
                "IEEE 1800-2023 19.5.7 compares the values of a bin as values of the type of the coverpoint; write the values that type has, or give the coverpoint a type that has them"
            }
        }))
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.location
            .as_ref()
            .map(|location| &location.source as &dyn miette::SourceCode)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        let location = self.location.as_ref()?;
        let label = match self.kind {
            FrontendWarningKind::CovergroupBinValue { .. } => "written here",
            FrontendWarningKind::PackedToUnpackedAssignment => "value assigned here",
            FrontendWarningKind::NonstandardForceTarget => "target selected here",
            _ => "called here",
        };
        Some(Box::new(std::iter::once(
            miette::LabeledSpan::new_with_span(Some(label.to_string()), location.span),
        )))
    }
}
