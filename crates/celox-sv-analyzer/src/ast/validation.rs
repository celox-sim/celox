//! Unsupported-construct checks and bounded procedural-loop validation.

use super::*;
use crate::source_tree::{Ranges, Starts};
use celox_sv_syntax::ExprId;

/// How an `always` construct is simulated.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AlwaysKind {
    Comb,
    Ff,
    /// A process that runs its statement forever, suspending at its timing
    /// controls (IEEE 1800-2023 9.2.2.1).
    Process,
}

/// `always_comb` and `always @*` are combinational; `always_ff` and an
/// `always` sensitive to clock edges are sequential, as long as their body
/// has no timing control of its own. Every other `always` is a process, and
/// so is an edge-sensitive `always` whose events are not edges of variables
/// (`ff_process::always_falls_back_to_process`). The frontend runs one whose
/// events are not a one-bit clock and resets as a process too.
/// `always_latch` has the execution and sensitivity rules of `always_comb`.
pub(super) fn always_kind(
    procedure: &celox_sv_syntax::Procedure,
    native: &celox_sv_syntax::Declarations,
) -> Converted<AlwaysKind> {
    use celox_sv_syntax::{
        EventControl, ProcedureKind, StatementKind, TimingControlKind, TimingEdge,
    };
    Ok(match procedure.kind {
        ProcedureKind::AlwaysComb | ProcedureKind::AlwaysLatch => AlwaysKind::Comb,
        ProcedureKind::AlwaysFf => AlwaysKind::Ff,
        ProcedureKind::Always => {
            let StatementKind::Timing { control, body } = native.statement(procedure.body).kind
            else {
                return Ok(AlwaysKind::Process);
            };
            let TimingControlKind::Event(event) = &native.timing_control(control).kind else {
                return Ok(AlwaysKind::Process);
            };
            if has_timing_control(body, native) {
                return Ok(AlwaysKind::Process);
            }
            match event {
                EventControl::Wildcard => AlwaysKind::Comb,
                // An `iff` event is judged when it occurs, which only the
                // process path does (IEEE 1800-2023 9.4.2.3).
                EventControl::Items(items) if items.iter().any(|item| item.iff.is_some()) => {
                    AlwaysKind::Process
                }
                EventControl::Items(items)
                    if items.iter().any(|item| item.edge != TimingEdge::Any) =>
                {
                    AlwaysKind::Ff
                }
                _ => AlwaysKind::Process,
            }
        }
        _ => {
            return Err(AnalyzerError::Parse(
                "native always procedure has a different construct class".into(),
            ));
        }
    })
}

/// Native statement traversal excludes intra-assignment controls, matching the
/// existing process-classification contract. Typed leaf restrictions remain in
/// their semantic binders.
fn has_timing_control(
    root: celox_sv_syntax::StatementId,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    use celox_sv_syntax::StatementKind;
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let statement = native.statement(id);
        if matches!(
            statement.kind,
            StatementKind::Timing { .. }
                | StatementKind::Assertion {
                    header: celox_sv_syntax::AssertionHeader::Concurrent { .. },
                    ..
                }
                | StatementKind::Parallel { .. }
                | StatementKind::Wait { .. }
                | StatementKind::WaitFork
                | StatementKind::WaitOrder { .. }
        ) {
            return true;
        }
        if !matches!(
            statement.kind,
            celox_sv_syntax::StatementKind::Unconverted { .. }
        ) {
            pending.extend(statement.kind.children());
        }
    }
    false
}

/// The native root a combinational process evaluates, without wildcard timing.
pub(super) fn always_comb_body(
    procedure: &celox_sv_syntax::Procedure,
    native: &celox_sv_syntax::Declarations,
) -> Converted<Option<celox_sv_syntax::StatementId>> {
    use celox_sv_syntax::{EventControl, ProcedureKind, StatementKind, TimingControlKind};
    if always_kind(procedure, native)? != AlwaysKind::Comb {
        return Ok(None);
    }
    if matches!(
        procedure.kind,
        ProcedureKind::AlwaysComb | ProcedureKind::AlwaysLatch
    ) {
        return Ok(Some(procedure.body));
    }
    let StatementKind::Timing { control, body } = native.statement(procedure.body).kind else {
        return Err(timing::disagreement());
    };
    if !matches!(
        native.timing_control(control).kind,
        TimingControlKind::Event(EventControl::Wildcard)
    ) {
        return Err(timing::disagreement());
    }
    Ok((!matches!(native.statement(body).kind, StatementKind::Empty)).then_some(body))
}

/// Check process restrictions on the selected native graph. Inactive generate
/// items never reach this entrypoint, and walking uses an explicit worklist.
fn validate_procedure(
    procedure: &celox_sv_syntax::Procedure,
    native: &celox_sv_syntax::Declarations,
) -> Converted<()> {
    use celox_sv_syntax::{EventControl, ProcedureKind, StatementKind, TimingControlKind};
    let kind = match procedure.kind {
        ProcedureKind::Initial => return Ok(()),
        ProcedureKind::Final => return validate_final(procedure.body, native),
        _ => always_kind(procedure, native)?,
    };
    let mut pending = vec![procedure.body];
    while let Some(id) = pending.pop() {
        let statement = native.statement(id);
        match &statement.kind {
            // A plain `always @*` that calls a timed task runs as a process,
            // which may assign nonblocking; timed task classification
            // rejects the assignment in one that stays combinational.
            StatementKind::Assignment {
                nonblocking: true, ..
            } if kind == AlwaysKind::Comb && procedure.kind != ProcedureKind::Always => {
                return Err(unsupported(
                    if procedure.kind == ProcedureKind::AlwaysLatch {
                        "nonblocking assignment inside always_latch"
                    } else {
                        "nonblocking assignment inside always_comb"
                    },
                ));
            }
            StatementKind::Timing { control, .. }
            | StatementKind::Assignment {
                event_control: Some(control),
                ..
            } if kind == AlwaysKind::Ff => {
                if let TimingControlKind::Event(EventControl::Items(items)) =
                    &native.timing_control(*control).kind
                    && items.iter().any(|item| item.iff.is_some())
                {
                    return Err(unsupported("iff-qualified always_ff event"));
                }
            }
            _ => {}
        }
        pending.extend(statement.kind.children());
    }
    Ok(())
}

/// Final restrictions are syntactic, so even branches later folded away
/// must obey the function-statement rules (IEEE 1800-2023 9.2.3, 13.4).
fn validate_final(
    root: celox_sv_syntax::StatementId,
    native: &celox_sv_syntax::Declarations,
) -> Converted<()> {
    use celox_sv_syntax::StatementKind;
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let statement = native.statement(id);
        let forbidden = match &statement.kind {
            StatementKind::Assignment {
                timing: Some(_), ..
            }
            | StatementKind::Assignment {
                event_control: Some(_),
                ..
            }
            | StatementKind::Timing { .. } => Some("timing control"),
            StatementKind::Wait { .. }
            | StatementKind::WaitFork
            | StatementKind::WaitOrder { .. } => Some("wait statement"),
            StatementKind::Parallel { join, .. } if join != "join_none" => Some("fork-join block"),
            // 13.4.4 permits join_none in functions. Its child statements
            // obey task rules rather than the parent's function restrictions.
            StatementKind::Parallel { .. } => continue,
            StatementKind::Assertion { keyword, .. } if keyword == "expect" => {
                Some("expect statement")
            }
            _ => None,
        };
        if let Some(construct) = forbidden {
            return Err(unsupported(format!("{construct} in a final procedure")));
        }
        pending.extend(statement.kind.children());
    }
    Ok(())
}

/// Root module inspection skips control branches. The subsequent active-item
/// inspection visits an opaque checker's complete captured syntax, matching the
/// existing validation contract without implementing checker elaboration.
fn validate_body_items(
    items: &[celox_sv_syntax::BodyItemId],
    native: &celox_sv_syntax::Declarations,
    traverse_controls: bool,
) -> Converted<()> {
    use celox_sv_syntax::BodyItemKind;
    let mut pending = items.iter().rev().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        match &native.body_item(id).kind {
            BodyItemKind::Procedure(id) => validate_procedure(native.procedure(*id), native)?,
            BodyItemKind::Assign(id) if native.assign_statement(*id).delay.is_some() => {
                return Err(unsupported("delayed continuous assignment"));
            }
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => {
                pending.extend(items.iter().rev().copied());
            }
            BodyItemKind::Unit(id) => {
                pending.extend(native.unit(*id).items.iter().rev().copied());
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } if traverse_controls => {
                pending.extend(otherwise);
                pending.push(*then_body);
            }
            BodyItemKind::Case { arms, .. } if traverse_controls => {
                pending.extend(arms.iter().rev().map(|arm| arm.body));
            }
            BodyItemKind::Loop { body, .. } if traverse_controls => pending.push(*body),
            // Ordinary module controls await elaboration. Nested units retain
            // direct declaration checks in the enclosing root's inspection.
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn reject_silently_ignored_constructs(
    syntax_tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_dimensions: &ScopedMap<VariableDimensions>,
    parameter_values: &HashMap<String, Expr>,
) -> Converted<()> {
    if let Ok(unit) = syntax_tree.root_unit() {
        validate_program_structure(unit, syntax_tree.declarations())?;
    }
    let mut dimensions =
        PackedDimensions::new(parameter_dimensions.clone(), const_env, type_aliases);
    dimensions.parameter_values = parameter_values.clone().into();
    validate_scope(None, syntax_tree, &dimensions)?;
    if syntax_tree
        .root_unit()
        .is_ok_and(|unit| unit.kind.is_module_like())
    {
        let active = generate::items(syntax_tree, const_env, type_aliases)?;
        let mut views = generate::ScopeViews::new(&dimensions);
        for item in &active {
            item.procedure(syntax_tree.declarations())?;
            item.continuous_assignment(syntax_tree.declarations())?;
            let _constant_functions = item.enter();
            let _interface_scope = item.interface_scope();
            validate_scope(Some(item), syntax_tree, views.dimensions(item))?;
        }
    }
    Ok(())
}

/// A program is a leaf: it shall not contain `always` procedures or
/// declarations or instances of modules, interfaces or other programs (IEEE
/// 1800-2023 24.3), generate blocks included. A program nested in a module
/// is implicitly instantiated there, which Celox does not do.
fn validate_program_structure(
    unit: &celox_sv_syntax::Unit,
    native: &celox_sv_syntax::Declarations,
) -> Converted<()> {
    use celox_sv_syntax::{BodyItemKind, ProcedureKind, UnitKind};
    let program = unit.kind == UnitKind::Program;
    let illegal = |item: String| {
        Err(AnalyzerError::IllegalProgramItem {
            program: instances::normalize_identifier(&unit.name.spelling),
            item,
        })
    };
    let mut pending = unit.items.iter().rev().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        match &native.body_item(id).kind {
            BodyItemKind::Procedure(procedure) if program => {
                let keyword = match native.procedure(*procedure).kind {
                    ProcedureKind::Always => "always",
                    ProcedureKind::AlwaysComb => "always_comb",
                    ProcedureKind::AlwaysLatch => "always_latch",
                    ProcedureKind::AlwaysFf => "always_ff",
                    ProcedureKind::Initial | ProcedureKind::Final => continue,
                };
                return illegal(format!("`{keyword}` procedure"));
            }
            BodyItemKind::Instance(_) if program => return illegal("an instance".into()),
            BodyItemKind::Unit(nested) => {
                let nested = native.unit(*nested);
                if program {
                    let kind = match nested.kind {
                        UnitKind::Module => "module",
                        UnitKind::Interface => "interface",
                        UnitKind::Package => "package",
                        UnitKind::Program => "program",
                    };
                    return illegal(format!(
                        "{kind} declaration `{}`",
                        instances::normalize_identifier(&nested.name.spelling)
                    ));
                }
                if nested.kind == UnitKind::Program {
                    return Err(unsupported("program declaration nested in a module"));
                }
            }
            BodyItemKind::Region(items) | BodyItemKind::Block { items, .. } => {
                pending.extend(items.iter().rev().copied());
            }
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                pending.extend(otherwise);
                pending.push(*then_body);
            }
            BodyItemKind::Case { arms, .. } => {
                pending.extend(arms.iter().rev().map(|arm| arm.body));
            }
            BodyItemKind::Loop { body, .. } => pending.push(*body),
            _ => {}
        }
    }
    Ok(())
}

fn contains(outer: celox_sv_syntax::TokenRange, inner: celox_sv_syntax::TokenRange) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

/// Whole-file facts the scope validation selects from, built once per file
/// so each scope costs what its own token ranges contain.
#[derive(Debug)]
pub(crate) struct Facts {
    /// Generate controls, whose bodies a module's own validation skips.
    controls: Ranges,
    /// The first token of the body item declaring each nested unit.
    unit_starts: HashMap<celox_sv_syntax::UnitId, usize>,
    /// Unit and port positions of every header port, in arena order.
    ports: Vec<(usize, usize)>,
    port_starts: Starts,
    /// Type and dimension positions of every packed dimension.
    packed_dimensions: Vec<(usize, usize)>,
    packed_dimension_starts: Starts,
    /// Dimensions of type arguments naming an alias, with that name.
    override_dimensions: Vec<(String, celox_sv_syntax::Dimension)>,
    override_dimension_starts: Starts,
    /// Value parameter defaults of declarations and unit headers.
    parameter_defaults: Ranges,
    /// Value parameter defaults of parameter declarations.
    declared_defaults: Vec<ExprId>,
    declared_default_ranges: Ranges,
    /// Procedures and subroutines.
    procedural: Ranges,
    /// Subroutines, void casts of discarded calls and indexed widths.
    cast_exclusions: Ranges,
    pub(super) callees: HashSet<ExprId>,
    /// Token spans of the callees of calls.
    pub(super) callee_spans: HashSet<(usize, usize)>,
    /// Expressions that are expression statements.
    pub(super) expression_statements: HashSet<ExprId>,
    /// Expressions that are constant values, and the roots they are under.
    constant_roots: Vec<ExprId>,
    constant_nodes: HashSet<ExprId>,
    /// Callee and first argument of every call with one.
    first_arguments: Vec<(ExprId, ExprId)>,
    first_argument_ranges: Ranges,
}

impl Facts {
    fn new(native: &celox_sv_syntax::Declarations) -> Self {
        use celox_sv_syntax::{BodyItemKind, ExprKind, ParameterKind, StatementKind};
        let controls = Ranges::new(native.body_items.iter().filter_map(|item| {
            matches!(
                item.kind,
                BodyItemKind::Conditional { .. }
                    | BodyItemKind::Case { .. }
                    | BodyItemKind::Loop { .. }
            )
            .then_some(item.tokens)
        }));
        let mut unit_starts = HashMap::default();
        for item in &native.body_items {
            if let BodyItemKind::Unit(id) = item.kind {
                unit_starts.entry(id).or_insert(item.tokens.start);
            }
        }
        let units = || native.units.iter().chain(&native.nested_units);
        let ports = units()
            .enumerate()
            .flat_map(|(unit, data)| (0..data.ports.len()).map(move |port| (unit, port)))
            .collect::<Vec<_>>();
        let port_starts = Starts::new(
            units()
                .flat_map(|unit| &unit.ports)
                .map(|port| port.tokens)
                .enumerate(),
        );
        let packed_dimensions = native
            .types
            .iter()
            .enumerate()
            .flat_map(|(ty, data)| {
                (0..data.packed_dimensions.len()).map(move |dimension| (ty, dimension))
            })
            .collect::<Vec<_>>();
        let packed_dimension_starts = Starts::new(
            native
                .types
                .iter()
                .flat_map(|data| &data.packed_dimensions)
                .map(|dimension| dimension.tokens)
                .enumerate(),
        );
        let override_dimensions = native
            .instantiations
            .iter()
            .flat_map(|instance| instance.parameters.iter().flatten())
            .filter_map(|argument| argument.value)
            .filter_map(|id| type_binding::argument_alias(id, native))
            .flat_map(|(name, dimensions)| {
                dimensions
                    .into_iter()
                    .map(move |dimension| (name.clone(), dimension))
            })
            .collect::<Vec<_>>();
        let override_dimension_starts = Starts::new(
            override_dimensions
                .iter()
                .map(|(_, dimension)| dimension.tokens)
                .enumerate(),
        );
        let declared = || {
            native
                .parameter_declarations
                .iter()
                .flat_map(|declaration| &declaration.parameters)
        };
        let headers = || units().flat_map(|unit| unit.parameters.iter().flatten());
        let parameter_defaults = Ranges::new(
            declared()
                .chain(headers())
                .filter_map(parameters::ownership::default)
                .map(|id| native.expression(id).tokens),
        );
        let declared_defaults = declared()
            .filter_map(parameters::ownership::default)
            .collect::<Vec<_>>();
        let declared_default_ranges = Ranges::new(
            declared_defaults
                .iter()
                .map(|id| native.expression(*id).tokens),
        );
        let procedural = Ranges::new(
            native
                .procedures
                .iter()
                .map(|procedure| procedure.tokens)
                .chain(
                    native
                        .subroutines
                        .iter()
                        .map(|subroutine| subroutine.tokens),
                ),
        );
        // Discarded calls own a void cast as statement syntax. The statement
        // binder validates this wrapper; it is not an integral value cast.
        let void_casts = native.statements.iter().filter_map(|statement| {
            let StatementKind::Expression(root) = statement.kind else {
                return None;
            };
            let ExprKind::Cast { target, .. } = native.expression(root).kind else {
                return None;
            };
            let ExprKind::Type(id) = native.expression(target).kind else {
                return None;
            };
            matches!(&native.data_type(id).kind, celox_sv_syntax::DataTypeKind::Builtin(name) if name == "void")
                .then_some(native.expression(root).tokens)
        });
        let indexed_widths =
            native
                .expression_nodes()
                .filter_map(|(_, expression)| match &expression.kind {
                    ExprKind::Select { dimension, .. }
                        if matches!(
                            dimension.kind,
                            celox_sv_syntax::DimensionKind::Indexed { .. }
                        ) =>
                    {
                        Some(dimension.tokens)
                    }
                    _ => None,
                });
        let cast_exclusions = Ranges::new(
            native
                .subroutines
                .iter()
                .map(|subroutine| subroutine.tokens)
                .chain(void_casts)
                .chain(indexed_widths),
        );
        let callees = native
            .expression_nodes()
            .filter_map(|(_, expression)| match expression.kind {
                ExprKind::Call { expression, .. } => Some(expression),
                _ => None,
            })
            .collect();
        let callee_spans = native
            .expression_nodes()
            .filter_map(|(_, expression)| match expression.kind {
                ExprKind::Call { expression, .. } => {
                    let tokens = native.expression(expression).tokens;
                    Some((tokens.start, tokens.end))
                }
                _ => None,
            })
            .collect();
        let expression_statements = native
            .statements
            .iter()
            .filter_map(|statement| match statement.kind {
                StatementKind::Expression(id) => Some(id),
                _ => None,
            })
            .collect();
        let mut constant_roots = declared()
            .chain(headers())
            .filter_map(|parameter| match parameter.kind {
                ParameterKind::Value { default, .. } => default,
                _ => None,
            })
            .collect::<Vec<_>>();
        for data in &native.types {
            for dimension in &data.packed_dimensions {
                dimension_expressions(dimension, &mut constant_roots);
            }
            if let celox_sv_syntax::DataTypeKind::Enum { items, .. } = &data.kind {
                constant_roots.extend(items.iter().filter_map(|item| item.value));
            }
        }
        // Unpacked declaration bounds and expression forms retain their own
        // constant-value operands, even inside otherwise runtime statements.
        for unit in units() {
            for port in &unit.ports {
                for dimension in &port.dimensions {
                    dimension_expressions(dimension, &mut constant_roots);
                }
            }
        }
        for declaration in &native.data_declarations {
            if let celox_sv_syntax::DataDeclaration::Variables(declaration)
            | celox_sv_syntax::DataDeclaration::Constructed(declaration) = declaration
            {
                for dimension in declaration
                    .variables
                    .iter()
                    .flat_map(|variable| &variable.dimensions)
                {
                    dimension_expressions(dimension, &mut constant_roots);
                }
            }
        }
        for declaration in &native.net_declarations {
            for dimension in declaration
                .variables
                .iter()
                .flat_map(|variable| &variable.dimensions)
            {
                dimension_expressions(dimension, &mut constant_roots);
            }
        }
        for (_, expression) in native.expression_nodes() {
            match &expression.kind {
                ExprKind::Replication { count, .. } => constant_roots.push(*count),
                ExprKind::Select { dimension, .. } | ExprKind::StreamSelect { dimension, .. } => {
                    match dimension.kind {
                        celox_sv_syntax::DimensionKind::Indexed { width, .. } => {
                            constant_roots.push(width)
                        }
                        celox_sv_syntax::DimensionKind::Range { left, right } => {
                            constant_roots.extend([left, right])
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        let mut constant = constant_roots.clone();
        let mut constant_nodes = HashSet::default();
        while let Some(id) = constant.pop() {
            if constant_nodes.insert(id) {
                constant.extend(parameters::skipped::children(id, native));
            }
        }
        let first_arguments = native
            .expression_nodes()
            .filter_map(|(_, expression)| match &expression.kind {
                ExprKind::Call {
                    expression: callee,
                    arguments,
                } => Some((*callee, arguments.first()?.value?)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let first_argument_ranges = Ranges::new(
            first_arguments
                .iter()
                .map(|(_, id)| native.expression(*id).tokens),
        );
        Self {
            controls,
            unit_starts,
            ports,
            port_starts,
            packed_dimensions,
            packed_dimension_starts,
            override_dimensions,
            override_dimension_starts,
            parameter_defaults,
            declared_defaults,
            declared_default_ranges,
            procedural,
            cast_exclusions,
            callees,
            callee_spans,
            expression_statements,
            constant_roots,
            constant_nodes,
            first_arguments,
            first_argument_ranges,
        }
    }

    pub(super) fn of(tree: &SyntaxTree) -> &Self {
        tree.spans()
            .validation
            .get_or_init(|| Self::new(tree.declarations()))
    }
}

/// Validate authored native owners, excluding generate branches until their
/// elaborated item carries the appropriate lexical environment.
fn validate_scope(
    item: Option<&generate::Item>,
    syntax_tree: &AnalysisContext<'_>,
    indexed_dimensions: &PackedDimensions,
) -> Converted<()> {
    use celox_sv_syntax::{BodyItemKind, DataTypeKind, DimensionKind, ExprKind};
    let native = syntax_tree.declarations();
    let spans = syntax_tree.spans();
    let facts = Facts::of(syntax_tree);
    let owner = item.map(|item| item.id);
    let is_module = owner.is_none()
        && syntax_tree
            .root_unit()
            .is_ok_and(|unit| unit.kind.is_module_like());
    let items = owner.map_or_else(|| syntax_tree.body_membership(), |id| Ok(vec![id]))?;
    validate_body_items(&items, native, !is_module)?;
    parameters::skipped::validate_scope(owner, is_module, syntax_tree, indexed_dimensions)?;
    let const_env = &indexed_dimensions.const_env;
    let type_aliases = &indexed_dimensions.type_aliases;
    let mut ranges = items
        .iter()
        .map(|id| native.body_item(*id).tokens)
        .collect::<Vec<_>>();
    if owner.is_none()
        && let Ok(unit) = syntax_tree.root_unit()
    {
        let start = match syntax_tree.scope_handle() {
            crate::source_tree::ScopeHandle::Compilation => unreachable!(),
            crate::source_tree::ScopeHandle::SourceUnit(id) => {
                native.source_items.iter().find_map(|item| {
                    (item.kind == celox_sv_syntax::SourceItemKind::Unit(id))
                        .then_some(item.tokens.start)
                })
            }
            crate::source_tree::ScopeHandle::Unit(id) => facts.unit_starts.get(&id).copied(),
        }
        .ok_or_else(|| AnalyzerError::Parse("native root authored owner is absent".into()))?;
        ranges.push(celox_sv_syntax::TokenRange {
            start,
            end: unit.header.end,
        });
    }
    let scope = Ranges::new(ranges.iter().copied());
    let no_controls = Ranges::default();
    let excluded = if is_module {
        &facts.controls
    } else {
        &no_controls
    };
    let selected = |tokens| scope.contains(tokens) && !excluded.contains(tokens);
    timescales::validate_queries(syntax_tree, &ranges, excluded)?;
    reject_constant_calls(
        native,
        &selected,
        &ranges,
        syntax_tree,
        indexed_dimensions,
        item,
    )?;
    let units = native
        .units
        .iter()
        .chain(&native.nested_units)
        .collect::<Vec<_>>();
    for index in facts.port_starts.within(&ranges) {
        let (unit, port) = facts.ports[index];
        let port = &units[unit].ports[port];
        if !selected(port.tokens) {
            continue;
        }
        if port
            .data_type
            .is_some_and(|id| matches!(native.data_type(id).kind, DataTypeKind::Enum { .. }))
        {
            return Err(unsupported("enum port"));
        }
        if let Some(net_type) = &port.net_type {
            validate_net_type(net_type)?;
        }
    }
    for index in spans.net_declarations.within(&ranges) {
        let net = &native.net_declarations[index];
        if !selected(net.tokens) {
            continue;
        }
        validate_net_type(&net.net_type)?;
        if net
            .variables
            .iter()
            .any(|variable| !variable.dimensions.is_empty() && variable.value.is_some())
        {
            return Err(unsupported("net declaration assignment"));
        }
    }
    for index in spans.body_items.within(&ranges) {
        let item = &native.body_items[index];
        if !selected(item.tokens) {
            continue;
        }
        if let BodyItemKind::Unconverted { category, .. } = &item.kind {
            let diagnostic = match category.as_str() {
                "bind" => Some("bind directive"),
                "interconnect" => Some("interconnect net declaration"),
                "specify" => Some("specify block"),
                "defparam" => Some("defparam assignment"),
                "assert" | "assume" | "cover" | "restrict" => {
                    let mut tokens = native.tokens[item.tokens.start..item.tokens.end]
                        .iter()
                        .filter(|token| !token.is_trivia())
                        .skip_while(|token| token.text != *category);
                    tokens.next();
                    Some(
                        if tokens
                            .next()
                            .is_some_and(|token| matches!(token.text.as_str(), "#" | "final"))
                        {
                            "module deferred immediate assertion"
                        } else {
                            "concurrent assertion"
                        },
                    )
                }
                "$fatal" | "$error" | "$warning" | "$info" => Some("elaboration system task"),
                "and" | "nand" | "or" | "nor" | "xor" | "xnor" | "buf" | "not" | "bufif0"
                | "bufif1" | "notif0" | "notif1" | "nmos" | "pmos" | "cmos" | "rnmos" | "rpmos"
                | "rcmos" | "tran" | "rtran" | "tranif0" | "tranif1" | "rtranif0" | "rtranif1"
                | "pullup" | "pulldown" => Some("gate primitive instantiation"),
                _ => None,
            };
            if let Some(diagnostic) = diagnostic {
                return Err(unsupported(diagnostic));
            }
        }
    }
    for index in spans.instantiations.within(&ranges) {
        let instance = &native.instantiations[index];
        if selected(instance.tokens)
            && instance
                .instances
                .iter()
                .flat_map(|instance| &instance.connections)
                .any(|connection| {
                    matches!(
                        connection.kind,
                        celox_sv_syntax::InstanceConnectionKind::Wildcard
                    )
                })
        {
            return Err(unsupported("wildcard port connection"));
        }
    }
    for id in spans.types.within(&ranges) {
        let data = native.data_type(id);
        if !selected(data.tokens) {
            continue;
        }
        if matches!(data.kind, DataTypeKind::Struct { .. })
            && type_binding::bind(id, syntax_tree, indexed_dimensions)?.is_none()
        {
            return Err(unsupported(
                "unpacked struct, union, or unsupported packed struct member",
            ));
        }
    }
    let override_dimensions = facts
        .override_dimension_starts
        .within(&ranges)
        .into_iter()
        .map(|index| &facts.override_dimensions[index])
        .filter(|(name, _)| type_aliases.contains_key(name))
        .map(|(_, dimension)| dimension);
    for dimension in facts
        .packed_dimension_starts
        .within(&ranges)
        .into_iter()
        .map(|index| {
            let (ty, dimension) = facts.packed_dimensions[index];
            &native.types[ty].packed_dimensions[dimension]
        })
        .chain(override_dimensions)
        .filter(|dimension| selected(dimension.tokens))
    {
        let DimensionKind::Range { left, right } = dimension.kind else {
            continue;
        };
        for id in [left, right] {
            if constant_binding::expression(id, native, syntax_tree, const_env, type_aliases)?
                .is_none()
            {
                return Err(unsupported("unsupported packed range"));
            }
        }
    }
    for id in spans.expressions.within(&ranges) {
        let expression = native.expression(id);
        if !selected(expression.tokens) {
            continue;
        }
        match &expression.kind {
            ExprKind::MinTypMax { .. } if !native.timing_controls.iter().any(|control|
                matches!(control.kind, celox_sv_syntax::TimingControlKind::Delay { value, .. } if contains(native.expression(value).tokens, expression.tokens))) => {
                return Err(unsupported("mintypmax expression"));
            }
            ExprKind::Select { dimension, .. } => {
                if let DimensionKind::Indexed { width, .. } = dimension.kind {
                    if facts.parameter_defaults.contains(dimension.tokens) { continue; }
                    // IEEE 1800-2023 11.5.1 permits a variable base. Its
                    // expression is bound after variable types are available;
                    // only the width requires constant validation here.
                    check_indexed_width(width, syntax_tree, indexed_dimensions)?;
                }
            }
            _ => {}
        }
    }
    let candidates = spans.expressions.within(&ranges);
    for id in &candidates {
        let expression = native.expression(*id);
        let celox_sv_syntax::ExprKind::Cast {
            target,
            expression: operand,
        } = expression.kind
        else {
            continue;
        };
        let at = expression.tokens.start;
        if scope.covers(at)
            && !excluded.covers(at)
            && !facts.cast_exclusions.covers(at)
            && !facts
                .declared_default_ranges
                .covering(at)
                .into_iter()
                .any(|index| {
                    parameters::skipped::needs_selected(
                        &syntax_tree.state,
                        facts.declared_defaults[index],
                        native,
                    )
                })
        {
            expression_binding::cast_target_native(
                target,
                operand,
                native,
                syntax_tree,
                indexed_dimensions,
            )?;
        }
    }
    let mut native_bits = candidates.into_iter().filter(|id| {
        let expression = native.expression(*id);
        let celox_sv_syntax::ExprKind::Call { expression: callee, .. } = expression.kind else { return false; };
        if !matches!(&native.expression(callee).kind, celox_sv_syntax::ExprKind::Name(name) if matches!(name.spelling.as_str(),"$bits"|"$size"|"$dimensions"|"$unpacked_dimensions"|"$left"|"$right"|"$low"|"$high"|"$increment"|"$isunbounded")) { return false; }
        let at = expression.tokens.start;
        scope.covers(at) && !excluded.covers(at) && !facts.procedural.covers(at)
    }).collect::<Vec<_>>();
    native_bits.sort_by_key(|id| native.expression(*id).tokens.start);
    for root in native_bits {
        if let celox_sv_syntax::ExprKind::Call {
            expression: callee, ..
        } = native.expression(root).kind
            && expression_binding::native_name(&syntax_tree.state, callee, native).as_deref()
                == Some("$isunbounded")
        {
            expression_binding::bind(root, syntax_tree, indexed_dimensions)?;
        } else {
            dimensions::query_value(root, syntax_tree, indexed_dimensions)?
                .ok_or_else(|| unsupported("operand of native type query"))?;
        }
    }

    Ok(())
}

fn validate_net_type(name: &str) -> Converted<()> {
    match name {
        "supply0" | "supply1" | "tri0" | "tri1" => Err(unsupported("pull or supply net type")),
        "trireg" => Err(unsupported("trireg charge storage")),
        _ => Ok(()),
    }
}

fn dimension_expressions(
    dimension: &celox_sv_syntax::Dimension,
    expressions: &mut Vec<celox_sv_syntax::ExprId>,
) {
    use celox_sv_syntax::DimensionKind;
    match dimension.kind {
        DimensionKind::Index(id) => expressions.push(id),
        DimensionKind::Range { left, right } => expressions.extend([left, right]),
        DimensionKind::Indexed { base, width, .. } => expressions.extend([base, width]),
        DimensionKind::Queue(id) => expressions.extend(id),
        _ => {}
    }
}

/// Data-query operands are unevaluated even when their enclosing owner is a
/// constant value. Native spans identify those operands without parser nodes.
fn reject_constant_calls(
    native: &celox_sv_syntax::Declarations,
    selected: &impl Fn(celox_sv_syntax::TokenRange) -> bool,
    ranges: &[celox_sv_syntax::TokenRange],
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    item: Option<&generate::Item>,
) -> Converted<()> {
    use celox_sv_syntax::ExprKind;
    let facts = Facts::of(tree);
    // Specparams are inert timing declarations until used, but IEEE 1800-2023
    // 6.20.5 explicitly forbids using them to assign value parameters.
    let mut specparams = HashSet::default();
    for index in tree.spans().body_items.within(ranges) {
        let body = &native.body_items[index];
        if !selected(body.tokens)
            || !matches!(&body.kind, celox_sv_syntax::BodyItemKind::Unconverted { category, .. } if category == "specparam")
        {
            continue;
        }
        let mut depth = 0usize;
        let mut previous = None;
        for token in native.tokens[body.tokens.start..body.tokens.end]
            .iter()
            .filter(|token| !token.is_trivia())
        {
            match token.text.as_str() {
                "[" | "(" | "{" => depth += 1,
                "]" | ")" | "}" => depth = depth.saturating_sub(1),
                "=" if depth == 0 => {
                    if let Some(name) = previous {
                        specparams.insert(name);
                    }
                }
                _ => {}
            }
            previous = token.identifier();
        }
    }
    if !specparams.is_empty() {
        let mut pending = facts.constant_roots.clone();
        let mut visited = HashSet::default();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) || !selected(native.expression(id).tokens) {
                continue;
            }
            if let ExprKind::Name(name) = &native.expression(id).kind
                && specparams.contains(name.value())
            {
                return Err(unsupported("specparam in value parameter"));
            }
            pending.extend(parameters::skipped::children(id, native));
        }
    }
    // Operands of data queries, whose callee names may depend on the scope.
    let unevaluated = |tokens| {
        facts
            .first_argument_ranges
            .containing(tokens)
            .into_iter()
            .any(|index| {
                expression_binding::native_name(&tree.state, facts.first_arguments[index].0, native)
                    .is_some_and(|name| {
                        SystemFunction::from_name(&name)
                            .is_some_and(SystemFunction::has_unevaluated_operand)
                    })
            })
    };
    for id in tree.spans().expressions.within(ranges) {
        let expression = native.expression(id);
        if !selected(expression.tokens)
            || !facts.constant_nodes.contains(&id)
            || unevaluated(expression.tokens)
        {
            continue;
        }
        let (name, arguments) = match &expression.kind {
            ExprKind::Call {
                expression,
                arguments,
            } => {
                let Some(name) = expression_binding::native_name(&tree.state, *expression, native)
                else {
                    continue;
                };
                (name, arguments.as_slice())
            }
            ExprKind::Name(name)
                if name.spelling.starts_with('$')
                    && !matches!(name.spelling.as_str(), "$" | "$unit" | "$root")
                    && !facts.callees.contains(&id) =>
            {
                (name.spelling.clone(), &[][..])
            }
            _ => continue,
        };
        if name.starts_with('$') {
            let presence = arguments
                .iter()
                .map(|argument| {
                    if argument.value.is_some() {
                        system_functions::Arg::Given
                    } else {
                        system_functions::Arg::Omitted
                    }
                })
                .collect::<Vec<_>>();
            let function = system_functions::check_function_call(
                &name,
                (!arguments.iter().any(|argument| argument.name.is_some()))
                    .then_some(presence.as_slice()),
            )?;
            if function.reads_simulation_state() {
                return Err(AnalyzerError::InvalidSystemTfCall {
                    name,
                    detail: "not permitted in a constant-value expression".into(),
                });
            }
        } else if !const_functions::is_constant_function(&tree.state.const_functions, &name) {
            if let Some(error) =
                const_functions::conversion_error(&tree.state.const_functions, &name)
            {
                return Err(error);
            }
            let context = dimensions::query_context_for_operand(id, tree, dimensions)?;
            // Generate imports can hide an outer constant function with a
            // package DPI function. Resolve that authored name in the active
            // item before consulting the qualified package declarations.
            let bound = item.map_or_else(|| name.clone(), |item| item.name(&name));
            let imported = scope::imported(&tree.state);
            if imported
                .dpi_imports
                .iter()
                .any(|import| import.name() == bound)
                || context.get(&name).is_some()
                || context.const_env.contains_key(&name)
                || context.parameter_values.contains_key(&name)
                // Package declarations are analyzed completely before their
                // symbols are exported, including unused parameter defaults.
                || tree.root_unit().map_or(true, |unit| unit.kind == celox_sv_syntax::UnitKind::Package)
            {
                return Err(unsupported("user constant function call"));
            }
        }
    }
    Ok(())
}

/// Reject an indexed part-select width that is not a positive constant.
fn check_indexed_width(
    native: celox_sv_syntax::ExprId,
    syntax_tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let mut context = dimensions.clone();
    context.constant_indexed_base = true;
    let value = constant_binding::indexed_expression(
        native,
        syntax_tree.declarations(),
        syntax_tree,
        &context,
    )?;
    let width = selects::indexed_expression_value(&syntax_tree.state, value, &context)
        .ok_or_else(|| unsupported("constant index expression"))?;
    match eval_ast_const_expr(&syntax_tree.state, &width, &dimensions.const_env) {
        Some(width) if width > 0 => Ok(()),
        Some(width) => Err(unsupported(format!("indexed part-select width {width}"))),
        None => Err(unsupported(
            "indexed part-select width that is not a positive constant",
        )),
    }
}

#[cfg(test)]
mod traversal_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn validates_native_owners_without_visiting_inactive_generate_bodies() {
        for (body, accepted) in [
            (
                "logic visible; if (0) begin : hidden always_latch visible <= 1; end",
                true,
            ),
            ("logic visible; always_latch visible <= 1;", false),
            (
                "logic visible; if (1) begin : active always_latch visible <= 1; end",
                false,
            ),
        ] {
            let source = format!("module Top; {body} endmodule");
            let tree = crate::source_tree::parse_scope_fixture(
                &source,
                Path::new("native_validation_scopes.sv"),
            )
            .unwrap();
            let result = reject_silently_ignored_constructs(
                &AnalysisContext::unspecialized(&tree),
                &HashMap::default(),
                &HashMap::default(),
                &ScopedMap::default(),
                &HashMap::default(),
            );
            if accepted {
                result.unwrap();
            } else {
                assert!(
                    matches!(result, Err(AnalyzerError::Unsupported(ref reason)) if reason == "nonblocking assignment inside always_latch")
                );
            }
        }
    }
}
