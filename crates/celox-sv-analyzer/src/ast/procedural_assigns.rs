//! Procedural continuous assignments of variables (IEEE 1800-2023 10.6.1).
//!
//! `assign v = e;` makes `v` follow `e` and overrides the procedural
//! assignments to `v`, until a `deassign v;` or another `assign` of `v`.
//! Each target has a hidden select variable: zero while no assignment is
//! active, else the number of the active `assign` statement. The statements
//! become ordinary ones,
//!
//! ```text
//! assign v = e;   (statement k)     select = k; v = e;
//! deassign v;                       select = 0;
//! v = x;   v <= x;                  if (select == 0) v = x;
//! ```
//!
//! and one process for each `assign` statement drives the variable while its
//! statement is the active one (4.9.2):
//!
//! ```text
//! forever
//!   if (select == k) begin v = e; @(select or e or v); end
//!   else @(select);
//! ```
//!
//! The driver waits on `v` too. A write the guards do not reach (a
//! nonblocking update scheduled before the `assign` executed, a delayed one,
//! an output argument, a concatenation, a hierarchical name in another
//! module) is overridden in the time step it happens.
use super::*;
use crate::procedural::{EventEdge, EventItemBase, LoopKind};

/// Call `f` with every name `expr` reads, the positions of its selects
/// included.
pub(super) fn reads(expr: &Expr, f: &mut impl FnMut(&str)) {
    fn position(expr: &ConstExpr, f: &mut impl FnMut(&str)) {
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            match expr {
                ConstExpr::Ident(name) => f(name),
                ConstExpr::Literal(_) => {}
                ConstExpr::Select { expr, bit } => pending.extend([&**expr, &**bit]),
                ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                    pending.extend([&**expr, &**msb, &**lsb])
                }
                ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
                    pending.push(expr)
                }
                ConstExpr::Function { args, .. } => pending.extend(args),
                ConstExpr::Binary { left, right, .. } => pending.extend([&**left, &**right]),
                ConstExpr::Mux {
                    condition,
                    then_expr,
                    else_expr,
                } => pending.extend([&**condition, &**then_expr, &**else_expr]),
            }
        }
    }
    let mut pending = vec![expr];
    while let Some(expr) = pending.pop() {
        match expr {
            Expr::Ident(name) => f(name),
            Expr::Literal(_) => {}
            Expr::Select { expr, msb, lsb, .. } => {
                position(msb, f);
                position(lsb, f);
                pending.push(expr);
            }
            Expr::Concat(parts) => pending.extend(parts),
            Expr::RepeatConcat { count, parts } => {
                position(count, f);
                pending.extend(parts);
            }
            Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => pending.push(expr),
            Expr::Binary { left, right, .. } => pending.extend([&**left, &**right]),
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => pending.extend([&**condition, &**then_expr, &**else_expr]),
            Expr::Call { args, .. } => pending.extend(args.iter().flatten()),
            Expr::Inside { expr, items } => {
                pending.push(expr);
                for item in items {
                    pending.extend(item.exprs());
                }
            }
        }
    }
}

/// Whether the value of `expr` can change: it reads a variable or calls a
/// function, which may read one.
fn varies(expr: &Expr) -> bool {
    let mut varies = false;
    reads(expr, &mut |_| varies = true);
    lifetimes::visit_calls(expr, &mut |_, _| varies = true);
    varies
}

fn select_literal(number: usize) -> Expr {
    Expr::Literal(format!("32'd{number}"))
}

fn select_is(select: &str, number: usize) -> Expr {
    Expr::Binary {
        left: Box::new(Expr::Ident(select.to_string())),
        op: BinaryOp::Eq,
        right: Box::new(select_literal(number)),
    }
}

fn set_select(select: &str, number: usize) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(select.to_string()),
        rhs: select_literal(number),
        nonblocking: false,
    }
}

fn any_change(expr: Expr) -> EventItemBase<Expr> {
    EventItemBase {
        edge: EventEdge::Any,
        expr,
        iff: None,
    }
}

/// One `assign` statement: the `number`th of its target.
struct Driver {
    select: String,
    number: usize,
    lhs: LValue,
    rhs: Expr,
    /// The process the statement is in.
    process: usize,
}

struct Lowering<'a> {
    /// The select variable of each target and the number of `assign`
    /// statements of the target seen so far.
    targets: &'a mut HashMap<String, (String, usize)>,
    drivers: Vec<Driver>,
    /// The process whose body is being rewritten.
    process: usize,
}

impl Lowering<'_> {
    /// Rewrite the statements that execute: the `assign` and `deassign`
    /// statements, and the assignments to their targets.
    fn block(&mut self, stmts: &mut Vec<Stmt>) {
        let mut lowered = Vec::with_capacity(stmts.len());
        for mut stmt in std::mem::take(stmts) {
            match stmt {
                Stmt::ProceduralAssign { lhs, rhs } => {
                    let (select, count) = self
                        .targets
                        .get_mut(lhs.name())
                        .expect("every target has a select");
                    *count += 1;
                    lowered.push(set_select(select, *count));
                    lowered.push(Stmt::Assign {
                        lhs: lhs.clone(),
                        rhs: rhs.clone(),
                        nonblocking: false,
                    });
                    self.drivers.push(Driver {
                        select: select.clone(),
                        number: *count,
                        lhs,
                        rhs,
                        process: self.process,
                    });
                }
                // A `deassign` of a variable no `assign` targets ends
                // nothing, and is dropped.
                Stmt::Deassign { lhs } => {
                    if let Some((select, _)) = self.targets.get(lhs.name()) {
                        lowered.push(set_select(select, 0));
                    }
                }
                Stmt::Assign { ref lhs, .. } if self.targets.contains_key(lhs.name()) => {
                    let (select, _) = &self.targets[lhs.name()];
                    lowered.push(Stmt::If {
                        condition: select_is(select, 0),
                        then_body: vec![stmt],
                        else_body: Vec::new(),
                    });
                }
                _ => {
                    self.children(&mut stmt, false);
                    lowered.push(stmt);
                }
            }
        }
        *stmts = lowered;
    }

    /// Rewrite the statements of an `@*`, which describe what the statements
    /// it covers read and do not execute: an `assign` reads its expression
    /// (9.4.2.2).
    fn described(&mut self, stmts: &mut Vec<Stmt>) {
        let mut lowered = Vec::with_capacity(stmts.len());
        for mut stmt in std::mem::take(stmts) {
            match stmt {
                Stmt::ProceduralAssign { lhs, rhs } => lowered.push(Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking: false,
                }),
                Stmt::Deassign { .. } => {}
                _ => {
                    self.children(&mut stmt, true);
                    lowered.push(stmt);
                }
            }
        }
        *stmts = lowered;
    }

    fn children(&mut self, stmt: &mut Stmt, described: bool) {
        let body = |lowering: &mut Self, stmts: &mut Vec<Stmt>| {
            if described {
                lowering.described(stmts)
            } else {
                lowering.block(stmts)
            }
        };
        match stmt {
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                body(self, then_body);
                body(self, else_body);
            }
            Stmt::Case { items, default, .. } => {
                for item in items {
                    body(self, &mut item.body);
                }
                if let Some(default) = default {
                    body(self, default);
                }
            }
            Stmt::Loop {
                init,
                step,
                body: stmts,
                ..
            } => {
                body(self, init);
                body(self, step);
                body(self, stmts);
            }
            Stmt::Fork { branches, .. } => {
                for branch in branches {
                    body(self, branch);
                }
            }
            Stmt::Block { body: stmts, .. } => body(self, stmts),
            Stmt::WaitImplicit { reads, .. } => self.described(reads),
            // The update of a delayed nonblocking assignment is judged when
            // it happens, by the driver.
            _ => {}
        }
    }
}

/// The target a statement list assigns that has a procedural continuous
/// assignment, if any.
fn written_target<'a>(
    stmts: &'a [Stmt],
    targets: &HashMap<String, (String, usize)>,
) -> Option<&'a str> {
    let mut found = None;
    for stmt in stmts {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::Assign { lhs, .. } if targets.contains_key(lhs.name()) => {
                found = Some(lhs.name())
            }
            Stmt::AssignConcat { parts, .. } => {
                if let Some(part) = parts.iter().find(|part| targets.contains_key(part.name())) {
                    found = Some(part.name());
                }
            }
            _ => {}
        });
    }
    found
}

/// The target that a statement list assigns through a call of one of
/// `writers`: the subroutines that assign a target, themselves or through a
/// call, each with that target.
fn called_writer<'a>(stmts: &[Stmt], writers: &'a HashMap<String, String>) -> Option<&'a str> {
    let mut found = None;
    for stmt in stmts {
        stmt.walk(&mut |stmt| {
            if let Stmt::Call { name, .. } = stmt
                && let Some(target) = writers.get(name)
            {
                found = Some(target.as_str());
            }
            for expr in lifetimes::stmt_exprs(stmt) {
                lifetimes::visit_calls(expr, &mut |callee, _| {
                    if let Callee::Subroutine(name) = callee
                        && let Some(target) = writers.get(name)
                    {
                        found = Some(target.as_str());
                    }
                });
            }
        });
    }
    found
}

/// The target an actual denotes, as a whole variable, a select of one, or
/// a part of a concatenation of variables: an actual a callee copies back
/// must be a valid left-hand side of a procedural assignment, which a
/// concatenation of variables is (IEEE 1800-2023 10.4, 13.5).
fn actual_target<'a>(
    expr: &'a Expr,
    targets: &HashMap<String, (String, usize)>,
) -> Option<&'a str> {
    match expr {
        Expr::Ident(name) => targets.get(name).map(|_| name.as_str()),
        Expr::Select { expr, .. } => actual_target(expr, targets),
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
            parts.iter().find_map(|part| actual_target(part, targets))
        }
        _ => None,
    }
}

/// The target a system function or object operation writes through an
/// argument it copies back, such as the scanned variables of `$sscanf`
/// (IEEE 1800-2023 21.3.4.3), if any.
fn system_actual<'a>(
    callee: &Callee,
    args: &'a [Option<Expr>],
    targets: &HashMap<String, (String, usize)>,
) -> Option<&'a str> {
    args.iter().enumerate().find_map(|(position, arg)| {
        let writes = match callee {
            Callee::System(function) => function.writes_argument(position),
            Callee::Object(op) => op.writes_argument(position),
            _ => false,
        };
        if !writes {
            return None;
        }
        arg.as_ref().and_then(|arg| actual_target(arg, targets))
    })
}

/// The target a call to `name` writes through an argument it copies back
/// (an `output`, `inout` or `ref` formal), if any.
fn called_actual<'a>(
    name: &str,
    args: &'a [Option<Expr>],
    directions: &HashMap<String, Vec<crate::procedural::ParamDirection>>,
    targets: &HashMap<String, (String, usize)>,
) -> Option<&'a str> {
    let directions = directions.get(name)?;
    args.iter().enumerate().find_map(|(index, arg)| {
        let direction = directions.get(index)?;
        if !direction.is_written() {
            return None;
        }
        arg.as_ref().and_then(|arg| actual_target(arg, targets))
    })
}

/// The target that a statement list assigns through a call argument a
/// callee copies back, with the formal directions of the subroutines. A
/// system function, system task or object operation copies back the
/// arguments its `writes_argument` metadata marks.
fn called_actual_target<'a>(
    stmts: &'a [Stmt],
    directions: &HashMap<String, Vec<crate::procedural::ParamDirection>>,
    targets: &HashMap<String, (String, usize)>,
) -> Option<&'a str> {
    let mut found = None;
    for stmt in stmts {
        stmt.walk(&mut |stmt| {
            if let Stmt::Call { name, args, .. } = stmt
                && let Some(target) = called_actual(name, args, directions, targets)
            {
                found = Some(target);
            }
            if let Stmt::SystemTask { task, args, .. } = stmt
                && let Some(target) = args.iter().enumerate().find_map(|(position, arg)| {
                    if !task.writes_argument(position) {
                        return None;
                    }
                    match arg {
                        crate::procedural::SystemTaskArg::Expr(arg) => actual_target(arg, targets),
                        _ => None,
                    }
                })
            {
                found = Some(target);
            }
            for expr in lifetimes::stmt_exprs(stmt) {
                lifetimes::visit_calls(expr, &mut |callee, args| {
                    let target = match callee {
                        Callee::Subroutine(name) => called_actual(name, args, directions, targets),
                        _ => system_actual(callee, args, targets),
                    };
                    if let Some(target) = target {
                        found = Some(target);
                    }
                });
            }
        });
    }
    found
}

/// Reject target `name` of an `assign` or `deassign` that is not a whole
/// packed integral variable of the module (IEEE 1800-2023 10.6.1).
fn check_target(
    name: String,
    dimensions: &PackedDimensions,
    types: &semantic_types::TyTable,
    ports: &[Port],
    signals: &[Signal],
) -> Converted<()> {
    let variable = if let Some(signal) = signals.iter().find(|signal| signal.name == name) {
        if signal.is_net {
            return Err(AnalyzerError::ProceduralNetAssignment { name });
        }
        signal.hierarchical.is_none() && signal.package_variable.is_none()
    } else if let Some(port) = ports.iter().find(|port| port.name == name) {
        if port.is_net {
            return Err(AnalyzerError::ProceduralNetAssignment { name });
        }
        true
    } else {
        false
    };
    // The type the variable was bound to tells whether it is integral
    // (6.11.1); a shape bound to none, by its kind of value.
    let integral = dimensions.get(&name).is_some_and(|shape| {
        if let Some(id) = shape.bound(types) {
            return types.is_integral(id);
        }
        !(shape.string
            || shape.real
            || shape.class.is_some()
            || shape.chandle
            || shape.container.is_some()
            || shape.virtual_interface.is_some()
            || !shape.unpacked.is_empty()
            || shape.unpacked_struct.is_some())
    });
    if !variable || !integral {
        return Err(unsupported(format!(
            "procedural continuous assignment to `{name}`, which is not a packed integral variable of the module"
        )));
    }
    Ok(())
}

pub(super) fn lower(
    dimensions: &PackedDimensions,
    types: &semantic_types::TyTable,
    ports: &[Port],
    signals: &mut Vec<Signal>,
    comb: &[CombProcess],
    ff: &mut [FfProcess],
    initial: &mut Vec<InitialProcess>,
    subroutines: &mut [Subroutine],
    scope_directions: &lifetimes::Directions,
) -> Converted<()> {
    // The statements are in the processes only (see
    // `BodyBuilder::procedural_assign_target`). Only an `assign` makes
    // its variable a target: a `deassign` without one ends nothing, so
    // the variable keeps its other drivers and the statement is dropped
    // below. Deassign statements still take the lowering there, so they
    // do not survive it.
    let mut names = Vec::new();
    let mut deassigned = Vec::new();
    for process in initial.iter() {
        for stmt in &process.body {
            stmt.walk(&mut |stmt| match stmt {
                Stmt::ProceduralAssign { lhs, .. } => {
                    if !names.iter().any(|name| name == lhs.name()) {
                        names.push(lhs.name().to_string());
                    }
                }
                Stmt::Deassign { lhs } => deassigned.push(lhs.name().to_string()),
                _ => {}
            });
        }
    }
    if names.is_empty() && deassigned.is_empty() {
        return Ok(());
    }
    // A `deassign` names a variable an `assign` may target (10.6.1), also
    // where no `assign` does and the statement ends nothing.
    for name in deassigned.iter().filter(|name| !names.contains(name)) {
        check_target(name.clone(), dimensions, types, ports, signals)?;
    }
    let mut targets: HashMap<String, (String, usize)> = HashMap::default();
    for name in names {
        check_target(name.clone(), dimensions, types, ports, signals)?;
        let mut serial = targets.len();
        let select = loop {
            let select = format!("%procedural_assign{serial}_select");
            if dimensions.get(&select).is_none()
                && !signals.iter().any(|signal| signal.name == select)
            {
                break select;
            }
            serial += 1;
        };
        let mut storage = Type::new(TypeKind::Bit);
        storage.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal("31".into()),
            ConstExpr::Literal("0".into()),
        ));
        signals.push(Signal::new(select.clone(), storage));
        targets.insert(name, (select, 0));
    }
    // A variable has continuous assignments or procedural ones, not both
    // (6.5), and only its own procedure writes a variable that `always_comb`,
    // `always_latch` or `always_ff` writes (9.2.2.2.1, 9.2.2.4). An `always
    // @*` that stays combinational has no assignment to guard. A procedure
    // also writes what a subroutine it calls assigns.
    // The formal directions of every subroutine a procedure may call: those
    // of the scope, and those it imports from packages and `$unit`.
    let mut directions = scope_directions.clone();
    directions.extend(subroutines.iter().map(|subroutine| {
        (
            subroutine.name.clone(),
            subroutine
                .params
                .iter()
                .map(|param| param.direction)
                .collect(),
        )
    }));
    let mut writers: HashMap<String, String> = HashMap::default();
    loop {
        let mut grew = false;
        for subroutine in subroutines.iter() {
            if writers.contains_key(&subroutine.name) {
                continue;
            }
            if let Some(target) = written_target(&subroutine.body, &targets)
                .or_else(|| called_writer(&subroutine.body, &writers))
                .or_else(|| called_actual_target(&subroutine.body, &directions, &targets))
            {
                let target = target.to_string();
                writers.insert(subroutine.name.clone(), target);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let combinational = comb
        .iter()
        .find_map(|process| {
            process
                .assignments
                .iter()
                .map(Assignment::lhs)
                .find(|name| targets.contains_key(*name))
                .or_else(|| written_target(&process.body, &targets))
                .or_else(|| called_writer(&process.body, &writers))
                .or_else(|| called_actual_target(&process.body, &directions, &targets))
        })
        .or_else(|| {
            ff.iter()
                .filter(|process| process.always_ff)
                .find_map(|process| {
                    written_target(&process.body, &targets)
                        .or_else(|| called_writer(&process.body, &writers))
                        .or_else(|| called_actual_target(&process.body, &directions, &targets))
                })
        });
    if let Some(name) = combinational {
        return Err(unsupported(format!(
            "procedural continuous assignment to `{name}`, which a continuous assignment or an `always_comb`, `always_latch`, `always_ff` or `always @*` procedure assigns"
        )));
    }
    let mut lowering = Lowering {
        targets: &mut targets,
        drivers: Vec::new(),
        process: 0,
    };
    for process in ff.iter_mut() {
        lowering.block(&mut process.body);
    }
    for subroutine in subroutines.iter_mut() {
        lowering.block(&mut subroutine.body);
    }
    for (index, process) in initial.iter_mut().enumerate() {
        lowering.process = index;
        lowering.block(&mut process.body);
    }
    for driver in lowering.drivers {
        let mut events = vec![any_change(Expr::Ident(driver.select.clone()))];
        if varies(&driver.rhs) {
            events.push(any_change(driver.rhs.clone()));
        }
        events.push(any_change(Expr::Ident(driver.lhs.name().to_string())));
        let owner = &initial[driver.process];
        let creation_order = owner.creation_order;
        let site = owner.site.clone().map(|mut site| {
            site.construct = "procedural continuous assignment";
            site
        });
        let condition = owner.condition.clone();
        initial.push(InitialProcess {
            creation_order,
            site,
            condition,
            body: vec![Stmt::Loop {
                kind: LoopKind::Forever,
                init: Vec::new(),
                condition: None,
                step: Vec::new(),
                body: vec![Stmt::If {
                    condition: select_is(&driver.select, driver.number),
                    then_body: vec![
                        Stmt::Assign {
                            lhs: driver.lhs,
                            rhs: driver.rhs,
                            nonblocking: false,
                        },
                        Stmt::WaitEvent(events),
                    ],
                    else_body: vec![Stmt::WaitEvent(vec![any_change(Expr::Ident(
                        driver.select,
                    ))])],
                }],
            }],
            kind: celox_design::ProcessKind::Regular,
            validation_body: Vec::new(),
            synthetic: true,
            repeats: false,
        });
    }
    Ok(())
}
