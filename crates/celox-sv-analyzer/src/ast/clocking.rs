//! Clocking blocks (IEEE 1800-2023 clause 14).
//!
//! A clocking block `cb` declares in its scope:
//! - `cb`, an event that the block's process triggers in the Observed region
//!   of each time slot in which its clocking event occurs: the clocking
//!   block event that `@(cb)` waits for (14.10);
//! - [`marker`], one more than the time of the latest clocking event, 0
//!   before the first; it also tells that `cb` names a clocking block;
//! - [`sampled`] for each input or inout clockvar: the value the block
//!   sampled at its latest clocking event (14.13);
//! - [`skewed`] for each input of a delay skew: the signal delayed by the
//!   skew, which a process of its own assigns with nonblocking assignments
//!   of that delay at each change of the signal, so that in the Observed
//!   region of a slot it holds the signal's final value of the slot the skew
//!   earlier;
//! - [`drives_issued`] and [`drives_matured`], the counts of the
//!   synchronous drives executed between clocking events and of those of
//!   them that matured, which keep their order (14.16.2).
//!
//! The process of the block waits for the clocking event, records its time,
//! waits for the Observed region, samples the inputs of `#1step` skew (their
//! values in the Preponed region of the slot, the steady-state values of
//! the previous one, 14.4), those of `#0` skew and the delayed signals of
//! the inputs of a delay skew, and triggers `cb`: the sampled values are
//! all updated together, before the clocking block event (14.13).
//!
//! An unnamed default clocking block and an unnamed global clocking
//! declaration declare nothing that a design can refer to: what they would
//! define, the cycles of `##` (14.11) and `$global_clock` (14.14), Celox does
//! not support.
use super::*;
use celox_sv_syntax::{
    ClockingDeclaration, ClockingDirection, ClockingId, ClockingItem, ClockingKind, ClockingSkew,
    SkewDelay,
};

/// When an input clockvar samples its signal (14.4, 14.13).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum InputSkew {
    /// `#1step`: the value at the end of the previous time slot.
    Step,
    /// `#0`: the value in the Observed region of the clocking event's slot.
    Zero,
    /// A delay, in the time unit of the clocking block's scope: the value at
    /// the end of the time slot the delay before the clocking event.
    Delay(celox_sv_syntax::ExprId),
}

/// When an output clockvar drives its signal after the clocking event
/// (14.4, 14.16).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OutputSkew {
    /// `#1step`, one step of the global time precision (3.14.3).
    Step,
    /// A delay, in the time unit of the clocking block's scope; `#0`
    /// included.
    Delay(celox_sv_syntax::ExprId),
    /// No skew: `#0`.
    Zero,
}

/// A clockvar of a clocking block.
#[derive(Clone, Debug)]
pub(super) struct Clockvar {
    pub(super) name: String,
    pub(super) input: Option<InputSkew>,
    pub(super) output: Option<OutputSkew>,
}

/// A clocking block, as far as Celox supports one.
#[derive(Clone, Debug)]
pub(super) struct Block<'a> {
    pub(super) name: String,
    pub(super) declaration: &'a ClockingDeclaration,
    pub(super) vars: Vec<Clockvar>,
}

/// The variable that holds one more than the time of the latest clocking
/// event of clocking block `block`.
pub(super) fn marker(block: &str) -> String {
    // `%` starts no identifier of the source (see `continuous_delays`).
    format!("%clocking${block}")
}

/// The count of the synchronous drives through clocking block `block` that
/// were executed between its clocking events, each of which takes the count
/// before it as its turn (see `procedural::synchronous_drives`).
pub(super) fn drives_issued(block: &str) -> String {
    format!("%drives_issued${block}")
}

/// The count of those drives that have matured: the turn of the next one.
pub(super) fn drives_matured(block: &str) -> String {
    format!("%drives_matured${block}")
}

/// The variable that holds the sampled value of input clockvar `var` of
/// clocking block `block`.
pub(super) fn sampled(block: &str, var: &str) -> String {
    format!("%clocking${block}${var}")
}

/// The variable that holds the signal of input clockvar `var` of clocking
/// block `block` delayed by its input skew.
pub(super) fn skewed(block: &str, var: &str) -> String {
    format!("%clocking${block}${var}$skew")
}

fn invalid(text: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidClocking(text.into())
}

/// The skew `skew` as an input skew.
fn input_skew(skew: &ClockingSkew, native: &celox_sv_syntax::Declarations) -> Converted<InputSkew> {
    if skew.edge.is_some() {
        return Err(unsupported("clocking input skew with an edge"));
    }
    match skew.delay {
        Some(SkewDelay::Step) => Ok(InputSkew::Step),
        Some(SkewDelay::Value(value)) if zero(value, native) => Ok(InputSkew::Zero),
        Some(SkewDelay::Value(value)) => Ok(InputSkew::Delay(value)),
        None => Err(unsupported("clocking input skew with an edge")),
    }
}

/// The skew `skew` as an output skew.
fn output_skew(
    skew: &ClockingSkew,
    native: &celox_sv_syntax::Declarations,
) -> Converted<OutputSkew> {
    if skew.edge.is_some() {
        return Err(unsupported("clocking output skew with an edge"));
    }
    match skew.delay {
        Some(SkewDelay::Step) => Ok(OutputSkew::Step),
        Some(SkewDelay::Value(value)) if zero(value, native) => Ok(OutputSkew::Zero),
        Some(SkewDelay::Value(value)) => Ok(OutputSkew::Delay(value)),
        None => Err(unsupported("clocking output skew with an edge")),
    }
}

/// Whether expression `value` is the literal 0.
fn zero(value: celox_sv_syntax::ExprId, native: &celox_sv_syntax::Declarations) -> bool {
    let value = expression_binding::ungroup(value, native);
    matches!(&native.expression(value).kind, celox_sv_syntax::ExprKind::Literal(text)
        if text.chars().all(|c| c == '0' || c == '_'))
}

/// The clocking block that clocking declaration `id` declares, checked
/// against the rules of clause 14 and the subset Celox supports; `None` for
/// a declaration that declares nothing a design can refer to (see the
/// module documentation).
pub(super) fn block(
    native: &celox_sv_syntax::Declarations,
    id: ClockingId,
) -> Converted<Option<Block<'_>>> {
    let declaration = native.clocking(id);
    // A generate construct makes a scope of each of its blocks (27.5).
    let tokens = declaration.tokens;
    if native.body_items.iter().any(|item| {
        matches!(
            item.kind,
            celox_sv_syntax::BodyItemKind::Block { .. }
                | celox_sv_syntax::BodyItemKind::Conditional { .. }
                | celox_sv_syntax::BodyItemKind::Case { .. }
                | celox_sv_syntax::BodyItemKind::Loop { .. }
        ) && item.tokens.start <= tokens.start
            && tokens.end <= item.tokens.end
    }) {
        return Err(unsupported("clocking block in a generate block"));
    }
    if declaration.kind == ClockingKind::DefaultReference {
        return Ok(None);
    }
    let Some(name) = declaration
        .name
        .as_ref()
        .map(|name| instances::normalize_identifier(&name.spelling))
    else {
        return match declaration.kind {
            ClockingKind::Block { default: false } => Err(invalid(
                "only a default clocking block may be unnamed (IEEE 1800-2023 14.3)",
            )),
            _ => Ok(None),
        };
    };
    // Unless specified, inputs sample at #1step and outputs drive at #0
    // (14.3).
    let mut default_input = InputSkew::Step;
    let mut default_output = OutputSkew::Zero;
    for item in &declaration.items {
        if let ClockingItem::DefaultSkew { input, output, .. } = item {
            if let Some(input) = input {
                default_input = input_skew(input, native)?;
            }
            if let Some(output) = output {
                default_output = output_skew(output, native)?;
            }
        }
    }
    let mut vars: Vec<Clockvar> = Vec::new();
    for item in &declaration.items {
        let (direction, signals) = match item {
            ClockingItem::DefaultSkew { .. } => continue,
            ClockingItem::Assertion { .. } => {
                return Err(unsupported(
                    "property, sequence or let declaration in a clocking block",
                ));
            }
            ClockingItem::Signals {
                direction, signals, ..
            } => (direction, signals),
        };
        let input = |skew: &Option<ClockingSkew>| match skew {
            Some(skew) => input_skew(skew, native),
            None => Ok(default_input),
        };
        let output = |skew: &Option<ClockingSkew>| match skew {
            Some(skew) => output_skew(skew, native),
            None => Ok(default_output),
        };
        let (input, output) = match direction {
            ClockingDirection::Input(skew) => (Some(input(skew)?), None),
            ClockingDirection::Output(skew) => (None, Some(output(skew)?)),
            ClockingDirection::InputOutput(input_skew, output_skew) => {
                (Some(input(input_skew)?), Some(output(output_skew)?))
            }
            ClockingDirection::Inout => (Some(default_input), Some(default_output)),
        };
        for signal in signals {
            if signal.expression.is_some() {
                return Err(unsupported(
                    "clockvar of a hierarchical expression (IEEE 1800-2023 14.5)",
                ));
            }
            let var = instances::normalize_identifier(&signal.name.spelling);
            if vars.iter().any(|known| known.name == var) {
                return Err(invalid(format!(
                    "clockvar `{var}` declared twice in clocking block `{name}`"
                )));
            }
            vars.push(Clockvar {
                name: var,
                input,
                output,
            });
        }
    }
    Ok(Some(Block {
        name,
        declaration,
        vars,
    }))
}

/// Check clocking declaration `id` against the other clocking declarations
/// of the scope being analyzed: one default clocking (14.12) and one global
/// clocking (14.14) at most, and a default clocking specification names a
/// clocking block of the scope.
pub(super) fn check_scope(tree: &AnalysisContext<'_>, id: ClockingId) -> Converted<()> {
    let native = tree.declarations();
    let unit = tree.root_unit()?.body;
    let declarations: Vec<&ClockingDeclaration> = native
        .body_items
        .iter()
        .filter_map(|item| match item.kind {
            celox_sv_syntax::BodyItemKind::Clocking(id)
                if unit.start <= item.tokens.start && item.tokens.end <= unit.end =>
            {
                Some(native.clocking(id))
            }
            _ => None,
        })
        .collect();
    let count = |default: bool| {
        declarations
            .iter()
            .filter(|declaration| match declaration.kind {
                ClockingKind::Block { default: true } | ClockingKind::DefaultReference => default,
                ClockingKind::Global => !default,
                ClockingKind::Block { default: false } => false,
            })
            .count()
    };
    if count(true) > 1 {
        return Err(invalid(
            "more than one default clocking in a scope (IEEE 1800-2023 14.12)",
        ));
    }
    if count(false) > 1 {
        return Err(invalid(
            "more than one global clocking declaration in a scope (IEEE 1800-2023 14.14)",
        ));
    }
    let declaration = native.clocking(id);
    if declaration.kind == ClockingKind::DefaultReference
        && let Some(name) = &declaration.name
    {
        let name = instances::normalize_identifier(&name.spelling);
        if !declarations.iter().any(|other| {
            other.kind == (ClockingKind::Block { default: false })
                && other
                    .name
                    .as_ref()
                    .is_some_and(|other| instances::normalize_identifier(&other.spelling) == name)
        }) {
            return Err(invalid(format!(
                "default clocking `{name}` names no clocking block of the scope (IEEE 1800-2023 14.12)"
            )));
        }
    }
    Ok(())
}

/// The type of the variable that holds the sampled values of a clockvar
/// whose signal has type `ty`.
fn sample_type(ty: &Type) -> Type {
    let mut ty = ty.clone();
    if ty.kind() == TypeKind::Implicit {
        ty.kind = TypeKind::Logic;
    }
    ty
}

/// The variables of clocking declaration `id` (see the module
/// documentation), or the one `selected` names. Each clockvar names a port
/// of the scope, in `ports`, or a variable declared before the clocking
/// block, in `declared`.
pub(super) fn signals(
    native: &celox_sv_syntax::Declarations,
    id: ClockingId,
    selected: Option<&str>,
    ports: &[Port],
    declared: &[Signal],
    table: &mut semantic_types::TyTable,
) -> Converted<Vec<Signal>> {
    let Some(block) = block(native, id)? else {
        return Ok(Vec::new());
    };
    let event = classes::handle(table, events::CLASS);
    let mut signals = Vec::new();
    let mut time = Type::new(TypeKind::Bit);
    time.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("64".into()),
        ConstExpr::Literal("0".into()),
    ));
    let mut push = |name: String, ty: Type| {
        if selected.is_none_or(|selected| selected == name) {
            signals.push(Signal::new(name, ty));
        }
    };
    push(block.name.clone(), event);
    push(marker(&block.name), time);
    let mut count = Type::new(TypeKind::Bit);
    count.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal("63".into()),
        ConstExpr::Literal("0".into()),
    ));
    push(drives_issued(&block.name), count.clone());
    push(drives_matured(&block.name), count);
    for var in &block.vars {
        if var.input.is_none() {
            continue;
        }
        let name = sampled(&block.name, &var.name);
        let skew = skewed(&block.name, &var.name);
        let delayed = matches!(var.input, Some(InputSkew::Delay(_)));
        if selected.is_some_and(|selected| selected != name && (!delayed || selected != skew)) {
            continue;
        }
        let ty = signal_type(&var.name, ports, declared).ok_or_else(|| {
            unsupported(format!(
                "clockvar `{}` of clocking block `{}` whose signal is not a port or a variable declared before the block",
                var.name, block.name
            ))
        })?;
        // Their sampled variables would be aggregates, whose storage is
        // their leaves (7.2, 7.4).
        if ty.is_unpacked_struct_in(table)
            || !ty.unpacked_ranges().is_empty()
            || ty.container().is_some()
        {
            return Err(unsupported(format!(
                "clockvar `{}.{}` of an unpacked structure, an unpacked array or a container",
                block.name, var.name
            )));
        }
        if delayed {
            push(skew, sample_type(ty));
        }
        push(name, sample_type(ty));
    }
    Ok(signals)
}

/// The type of signal `name`, a port in `ports` or a variable in `declared`.
pub(super) fn signal_type<'a>(
    name: &str,
    ports: &'a [Port],
    declared: &'a [Signal],
) -> Option<&'a Type> {
    ports
        .iter()
        .find(|port| port.name() == name)
        .map(Port::r#type)
        .or_else(|| {
            declared
                .iter()
                .find(|signal| signal.name() == name)
                .map(Signal::r#type)
        })
}

/// The simulation time in ticks of the global time precision.
pub(super) fn now() -> Expr {
    Expr::Call {
        callee: system_functions::Callee::System(system_functions::SystemFunction::Time),
        // In ticks of the global precision, which only compare here.
        args: vec![Some(Expr::Literal("64'd1".into()))],
        arg_order: Vec::new(),
    }
}

/// Whether the latest clocking event of clocking block `block` occurred in
/// the current time slot.
pub(super) fn occurred(block: &str) -> Expr {
    Expr::Binary {
        left: Box::new(Expr::Ident(marker(block))),
        op: BinaryOp::Eq,
        right: Box::new(Expr::Binary {
            left: Box::new(now()),
            op: BinaryOp::Add,
            right: Box::new(Expr::Literal("65'd1".into())),
        }),
    }
}

fn assign(name: String, rhs: Expr) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(name),
        rhs,
        nonblocking: false,
    }
}

/// The body of the process of clocking block `block`, which `wait` waits
/// for the clocking event of (see the module documentation): one round,
/// which the process repeats.
pub(super) fn process(block: &Block<'_>, wait: Stmt) -> Vec<Stmt> {
    let mut body = vec![
        wait,
        assign(
            marker(&block.name),
            Expr::Binary {
                left: Box::new(now()),
                op: BinaryOp::Add,
                right: Box::new(Expr::Literal("65'd1".into())),
            },
        ),
    ];
    let inputs = |skew: InputSkew| {
        block
            .vars
            .iter()
            .filter(move |var| var.input == Some(skew))
            .map(|var| (sampled(&block.name, &var.name), var.name.clone()))
    };
    // Every clockvar takes its sample in the Observed region, where it is
    // published together with the others before the clocking block event
    // (14.13). The value at the end of the previous time slot, of a
    // `#1step` input, is the one in the Preponed region of this one (14.4,
    // 4.4.2.1), which `$sampled` gives all through the time slot.
    body.push(Stmt::AssertionRegion { reactive: false });
    body.extend(inputs(InputSkew::Step).map(|(target, signal)| {
        assign(
            target,
            Expr::Call {
                callee: system_functions::Callee::System(system_functions::SystemFunction::Sampled),
                args: vec![Some(Expr::Ident(signal))],
                arg_order: Vec::new(),
            },
        )
    }));
    body.extend(
        inputs(InputSkew::Zero).map(|(target, signal)| assign(target, Expr::Ident(signal))),
    );
    body.extend(
        block
            .vars
            .iter()
            .filter(|var| matches!(var.input, Some(InputSkew::Delay(_))))
            .map(|var| {
                assign(
                    sampled(&block.name, &var.name),
                    Expr::Ident(skewed(&block.name, &var.name)),
                )
            }),
    );
    body.push(Stmt::Eval(events::call(
        celox_design::ProcessOp::Trigger,
        vec![Expr::Ident(block.name.clone())],
    )));
    body
}

/// Whether `name` names a clocking block of the scope, not a declaration
/// that hides it.
pub(super) fn is_block(
    name: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> bool {
    dimensions.get(&marker(name)).is_some()
        && dimensions.get(name).is_some_and(|shape| {
            expression_binding::objects::shape_class(shape, tree, session, dimensions).as_deref()
                == Some(events::CLASS)
        })
}

/// The clocking block and clockvar that native expression `id` names
/// through a member, `cb.x`, and the members after it, outermost last.
pub(super) fn clockvar_path(
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Option<(String, String, Vec<String>)> {
    use celox_sv_syntax::ExprKind;
    let mut members = Vec::new();
    let mut root = expression_binding::ungroup(id, native);
    loop {
        match &native.expression(root).kind {
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "." => {
                members.push(instances::normalize_identifier(&name.spelling));
                root = expression_binding::ungroup(*expression, native);
            }
            ExprKind::Name(name) => {
                let block = instances::normalize_identifier(&name.spelling);
                let var = members.pop()?;
                members.reverse();
                return is_block(&block, tree, session, dimensions)
                    .then_some((block, var, members));
            }
            _ => return None,
        }
    }
}

/// The variable that a read of clockvar `var` of clocking block `block`
/// reads: its sampled value (14.13). Reading an output clockvar is illegal
/// (14.3).
pub(super) fn read(block: &str, var: &str, dimensions: &PackedDimensions) -> Converted<String> {
    let name = sampled(block, var);
    if dimensions.get(&name).is_none() {
        return Err(invalid(format!(
            "`{block}.{var}` is no input clockvar of clocking block `{block}`, which it would have to be to be read (IEEE 1800-2023 14.3)"
        )));
    }
    Ok(name)
}

/// Reject a write to a clockvar other than a synchronous drive (14.16).
pub(super) fn reject_write(
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let mut root = expression_binding::ungroup(id, native);
    while let celox_sv_syntax::ExprKind::Select { expression, .. } = &native.expression(root).kind {
        root = expression_binding::ungroup(*expression, native);
    }
    reject_block_use(root, native, tree, session, dimensions)?;
    match clockvar_path(root, native, tree, session, dimensions) {
        Some(_) if session.clockvar_drive == Some(root) => Ok(()),
        Some((block, var, _)) => Err(invalid(format!(
            "write to clockvar `{block}.{var}` other than by a synchronous drive (IEEE 1800-2023 14.16)"
        ))),
        None => Ok(()),
    }
}

/// The clocking block `name` that the scope being analyzed declares.
pub(super) fn declared<'a>(
    tree: &'a AnalysisContext<'_>,
    name: &str,
) -> Converted<Option<Block<'a>>> {
    let native = tree.declarations();
    let unit = tree.root_unit()?.body;
    for item in &native.body_items {
        if let celox_sv_syntax::BodyItemKind::Clocking(id) = item.kind
            && unit.start <= item.tokens.start
            && item.tokens.end <= unit.end
            && native
                .clocking(id)
                .name
                .as_ref()
                .is_some_and(|declared| instances::normalize_identifier(&declared.spelling) == name)
        {
            return block(native, id);
        }
    }
    Ok(None)
}

/// The global precision ticks of output skew `skew` (3.14.3, 14.4).
pub(super) fn skew_ticks(
    skew: OutputSkew,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<u64> {
    match skew {
        OutputSkew::Zero => Ok(0),
        OutputSkew::Step => Ok(1),
        OutputSkew::Delay(value) => delay_ticks(value, tree, session, dimensions),
    }
}

/// The global precision ticks of skew delay `value` (14.4).
pub(super) fn delay_ticks(
    value: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<u64> {
    let native = tree.declarations();
    let mut id = expression_binding::ungroup(value, native);
    if let celox_sv_syntax::ExprKind::MinTypMax { typical, .. } = native.expression(id).kind {
        id = typical;
    }
    if let celox_sv_syntax::ExprKind::Literal(text) = &native.expression(id).kind {
        if let Some(ticks) = timescales::delay_literal(id, text, tree)? {
            return Ok(ticks);
        }
        if let Some(ticks) = timescales::fixed_point_delay(id, text, tree)? {
            return Ok(ticks);
        }
    }
    let Some(value) = constant_binding::bind_in(id, native, tree, session, dimensions)?
        .and_then(|expr| eval_ast_const_expr(&tree.functions, &expr, &dimensions.const_env))
    else {
        // A real-valued constant other than a literal has no integral value.
        return Err(unsupported(
            "clocking skew that is neither an integral constant nor a time or fixed-point literal",
        ));
    };
    u64::try_from(value)
        .ok()
        .and_then(|value| value.checked_mul(timescales::time_divisor(id, tree).ok()?))
        .ok_or_else(|| {
            invalid("a clocking skew shall be a constant, non-negative delay (IEEE 1800-2023 14.4)")
        })
}

/// Reject a use of a clocking block's name as an event variable, which it
/// is only to Celox: a trigger or an assignment of the clocking block event
/// (14.10).
pub(super) fn reject_block_use(
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    let id = expression_binding::ungroup(id, native);
    if let celox_sv_syntax::ExprKind::Name(name) = &native.expression(id).kind {
        let name = instances::normalize_identifier(&name.spelling);
        if is_block(&name, tree, session, dimensions) {
            return Err(invalid(format!(
                "clocking block `{name}` is no event variable: it can only be waited for (IEEE 1800-2023 14.10)"
            )));
        }
    }
    Ok(())
}

/// The body of the process that delays the signal of input clockvar `var`
/// of clocking block `block` by its skew of `ticks` global precision ticks
/// (see the module documentation): its first statement, which assigns the
/// signal's value at the start, and one round, which the process repeats.
pub(super) fn skew_process(block: &str, var: &str, ticks: u64) -> (Stmt, Vec<Stmt>) {
    let delayed = || Stmt::DelayedNonblocking {
        control: Box::new(Stmt::Delay {
            amount: Expr::Literal(format!("64'd{ticks}")),
            ticks_per_unit: 1,
            ticks_per_precision: 1,
            precision_steps_per_tick: 1,
        }),
        assignment: Box::new(Stmt::Assign {
            lhs: LValue::Ident(skewed(block, var)),
            rhs: Expr::Ident(var.to_string()),
            nonblocking: true,
        }),
        re_nba: false,
    };
    let change = Stmt::WaitEvent(vec![crate::procedural::EventItemBase {
        edge: crate::procedural::EventEdge::Any,
        expr: Expr::Ident(var.to_string()),
        iff: None,
    }]);
    (delayed(), vec![change, delayed()])
}
