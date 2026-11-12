//! Procedural force and release of variables and nets (IEEE 1800-2023
//! 10.6.2).
//!
//! `force t = e;` makes `t` take `e` until `release t;`, overriding every
//! procedural assignment, continuous assignment and port driver of `t`.
//! Each forced target `t` has two hidden variables that the frontend reads
//! wherever `t` is written: the enable `E`, and the forced value `V`. While
//! `E` is set, every write of `t` stores `V` instead of its value (see
//! [`crate::ir::ForceControl`]). The statements become
//!
//! ```text
//! force t = e;   (statement k)     select = k or 0; V = e; E = 1; force-store t = V;
//! release t;                       select = 0; E = 0;
//! ```
//!
//! where the force-store puts the value in `t` at once and is no driver of
//! `t`. Only a target that procedures write, or an input port, is
//! force-stored: the writes of a continuous assignment or a port
//! connection depend on `E`, so they store `V` while it is set, and the
//! frontend gives a net that nothing else drives the driver `E ? V : 'z`
//! (see [`crate::ir::ForceControl::stored`]). A released variable keeps its
//! value until its next assignment; a released net, and a continuously
//! assigned variable, take the value of their drivers again. A target that a force
//! with a varying expression has, has a select variable (zero, or the
//! number of the active statement) and one process that keeps `V` and `t`
//! on the expression of the active statement (4.9.2):
//!
//! ```text
//! forever
//!   if (select == k) begin V = e; force-store t = V; @(select or e); end
//!   ...
//!   else @(select);
//! ```
use super::*;
use crate::ir::{ForceControl, ForceRole};
use crate::procedural::{EventEdge, EventItemBase, LoopKind};

/// The force controls of one target.
struct Controls {
    enable: String,
    value: String,
    /// The select variable, for a target with a force of a varying
    /// expression, and the number of such forces seen so far.
    select: Option<(String, usize)>,
    /// Whether a force stores its value into the target.
    stored: bool,
}

/// A force of a varying expression.
struct Driver {
    target: String,
    number: usize,
    rhs: Expr,
    /// The process the statement is in.
    process: usize,
}

fn literal(value: &str) -> Expr {
    Expr::Literal(value.to_string())
}

fn assign(name: &str, rhs: Expr) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(name.to_string()),
        rhs,
        nonblocking: false,
    }
}

fn select_literal(number: usize) -> Expr {
    literal(&format!("32'd{number}"))
}

fn select_is(select: &str, number: usize) -> Expr {
    Expr::Binary {
        left: Box::new(Expr::Ident(select.to_string())),
        op: BinaryOp::Eq,
        right: Box::new(select_literal(number)),
    }
}

fn any_change(expr: Expr) -> EventItemBase<Expr> {
    EventItemBase {
        edge: EventEdge::Any,
        expr,
        iff: None,
    }
}

/// Whether the value of `expr` can change: it reads a variable.
fn varies(expr: &Expr) -> bool {
    let mut varies = false;
    procedural_assigns::reads(expr, &mut |_| varies = true);
    varies
}

struct Lowering<'a> {
    controls: &'a mut HashMap<String, Controls>,
    drivers: Vec<Driver>,
    process: usize,
}

impl Lowering<'_> {
    fn block(&mut self, stmts: &mut Vec<Stmt>) {
        let mut lowered = Vec::with_capacity(stmts.len());
        for mut stmt in std::mem::take(stmts) {
            match stmt {
                Stmt::Force { lhs, rhs } => {
                    let target = lhs.name().to_string();
                    let controls = self
                        .controls
                        .get_mut(&target)
                        .expect("every target has controls");
                    if let Some((select, count)) = &mut controls.select {
                        let number = if varies(&rhs) {
                            *count += 1;
                            self.drivers.push(Driver {
                                target: target.clone(),
                                number: *count,
                                rhs: rhs.clone(),
                                process: self.process,
                            });
                            *count
                        } else {
                            0
                        };
                        lowered.push(assign(select, select_literal(number)));
                    }
                    lowered.push(assign(&controls.value, rhs));
                    lowered.push(assign(&controls.enable, literal("1'b1")));
                    if controls.stored {
                        lowered.push(Stmt::ForceStore {
                            lhs,
                            rhs: Expr::Ident(controls.value.clone()),
                        });
                    }
                }
                Stmt::Release { lhs } => {
                    // A target nothing forces is released by nothing.
                    let Some(controls) = self.controls.get(lhs.name()) else {
                        continue;
                    };
                    if let Some((select, _)) = &controls.select {
                        lowered.push(assign(select, select_literal(0)));
                    }
                    // A released net, and a continuously assigned variable,
                    // take the value of their drivers at once (10.6.2):
                    // the writes of the drivers depend on the enable.
                    lowered.push(assign(&controls.enable, literal("1'b0")));
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
    /// it covers read: a force reads its expression (9.4.2.2).
    fn described(&mut self, stmts: &mut Vec<Stmt>) {
        let mut lowered = Vec::with_capacity(stmts.len());
        for mut stmt in std::mem::take(stmts) {
            match stmt {
                Stmt::Force { lhs, rhs } => lowered.push(Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking: false,
                }),
                Stmt::Release { .. } => {}
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
            _ => {}
        }
    }
}

/// Whether `stmts` assign `name`, also through a concatenation.
fn writes(stmts: &[Stmt], name: &str) -> bool {
    let mut found = false;
    for stmt in stmts {
        stmt.walk(&mut |stmt| match stmt {
            Stmt::Assign { lhs, .. } => found |= lhs.name() == name,
            Stmt::AssignConcat { parts, .. } => {
                found |= parts.iter().any(|part| part.name() == name)
            }
            _ => {}
        });
    }
    found
}

/// Reject target `name` of a `force` or `release` that this lowering does
/// not support, and tell whether a force stores its value into it (see
/// [`crate::ir::ForceControl::stored`]).
fn check_target(
    name: &str,
    dimensions: &PackedDimensions,
    ports: &[Port],
    signals: &[Signal],
    comb: &[CombProcess],
    ff: &[FfProcess],
    initial: &[InitialProcess],
) -> Converted<bool> {
    let port = ports.iter().find(|port| port.name() == name);
    let net = if let Some(signal) = signals.iter().find(|signal| signal.name() == name) {
        if signal.hierarchical().is_some() {
            return Err(unsupported(format!(
                "force or release of `{name}`, a variable or net of another instance"
            )));
        }
        if signal.package_variable().is_some() {
            return Err(unsupported(format!(
                "force or release of `{name}`, a variable of a package"
            )));
        }
        signal.is_net()
    } else if let Some(port) = port {
        port.is_net
    } else {
        return Err(unsupported(format!(
            "force or release of `{name}`, which is not a variable or net of the module"
        )));
    };
    let integral = dimensions.get(name).is_some_and(|shape| {
        !(shape.string
            || shape.real
            || shape.class.is_some()
            || shape.chandle
            || shape.container.is_some()
            || shape.virtual_interface.is_some()
            || !shape.unpacked.is_empty()
            || shape.unpacked_struct.is_some())
    });
    if !integral {
        return Err(unsupported(format!(
            "force or release of `{name}`, which is not of a packed integral type"
        )));
    }
    // A target such a procedure writes keeps its forced value after the
    // release until the procedure executes again (9.2.2.2), which is not
    // supported yet.
    if let Some(process) = comb.iter().find(|process| {
        process.kind != CombProcessKind::ContinuousAssign
            && (process
                .assignments
                .iter()
                .any(|assignment| assignment.lhs.name() == name)
                || writes(&process.body, name))
    }) {
        let procedure = match process.kind {
            CombProcessKind::AlwaysLatch => "`always_latch`",
            CombProcessKind::AlwaysStar => "`always @*`",
            _ => "`always_comb`",
        };
        return Err(unsupported(format!(
            "force or release of `{name}`, which an {procedure} procedure assigns"
        )));
    }
    // A delayed continuous assignment drives its target from a process,
    // whose writes do not depend on the force.
    if initial
        .iter()
        .any(|process| process.synthetic && writes(&process.body, name))
    {
        return Err(unsupported(format!(
            "force or release of `{name}`, which a delayed continuous assignment or a net delay drives"
        )));
    }
    // A delayed nonblocking assignment lands when its delay ends, which
    // the force does not control.
    let delayed = |stmts: &[Stmt]| {
        let mut found = false;
        for stmt in stmts {
            stmt.walk(&mut |stmt| {
                if let Stmt::DelayedNonblocking { assignment, .. } = stmt {
                    found |= writes(std::slice::from_ref(&**assignment), name);
                }
            });
        }
        found
    };
    if ff.iter().any(|process| delayed(&process.body))
        || initial.iter().any(|process| delayed(&process.body))
    {
        return Err(unsupported(format!(
            "force or release of `{name}`, which a nonblocking assignment with an intra-assignment delay writes"
        )));
    }
    let input = port
        .is_some_and(|port| matches!(port.direction, PortDirection::Input | PortDirection::Inout));
    let assigned = comb.iter().any(|process| {
        process.kind == CombProcessKind::ContinuousAssign
            && (process
                .assignments
                .iter()
                .any(|assignment| assignment.lhs.name() == name)
                || writes(&process.body, name))
    });
    Ok(input || (!net && !assigned))
}

/// A name for a hidden variable that no declaration of the module has.
fn fresh(prefix: &str, serial: usize, dimensions: &PackedDimensions, signals: &[Signal]) -> String {
    let mut serial = serial;
    loop {
        let name = format!("{prefix}{serial}");
        if dimensions.get(&name).is_none() && !signals.iter().any(|signal| signal.name() == name) {
            return name;
        }
        serial += 1;
    }
}

fn bit_type(width: usize) -> Type {
    let mut storage = Type::new(TypeKind::Bit);
    if width > 1 {
        storage.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal((width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        ));
    }
    storage
}

/// Lower the `force` and `release` statements of the module's processes,
/// and return the enable of each forced target.
pub(super) fn lower(
    dimensions: &PackedDimensions,
    ports: &[Port],
    signals: &mut Vec<Signal>,
    comb: &[CombProcess],
    ff: &[FfProcess],
    initial: &mut Vec<InitialProcess>,
) -> Converted<HashMap<String, String>> {
    // The statements are in the processes only (see
    // `BodyBuilder::force_target`).
    let mut names: Vec<String> = Vec::new();
    let mut varying: HashSet<String> = HashSet::default();
    for process in initial.iter() {
        for stmt in &process.body {
            stmt.walk(&mut |stmt| match stmt {
                Stmt::Force { lhs, rhs } => {
                    if !names.iter().any(|name| name == lhs.name()) {
                        names.push(lhs.name().to_string());
                    }
                    if varies(rhs) {
                        varying.insert(lhs.name().to_string());
                    }
                }
                Stmt::Release { lhs } if !names.iter().any(|name| name == lhs.name()) => {
                    names.push(lhs.name().to_string());
                }
                _ => {}
            });
        }
    }
    if names.is_empty() {
        return Ok(HashMap::default());
    }
    let mut controls: HashMap<String, Controls> = HashMap::default();
    for (serial, name) in names.into_iter().enumerate() {
        let stored = check_target(&name, dimensions, ports, signals, comb, ff, initial)?;
        let target_type = signals
            .iter()
            .find(|signal| signal.name() == name)
            .map(|signal| signal.r#type().clone())
            .or_else(|| {
                ports
                    .iter()
                    .find(|port| port.name() == name)
                    .map(|port| port.r#type().clone())
            })
            .expect("a checked target is a signal or a port");
        let enable = fresh("%force_enable", serial, dimensions, signals);
        signals.push(Signal::force_control(
            enable.clone(),
            bit_type(1),
            ForceControl {
                target: name.clone(),
                role: ForceRole::Enable,
                stored,
            },
        ));
        let value = fresh("%force_value", serial, dimensions, signals);
        signals.push(Signal::force_control(
            value.clone(),
            target_type,
            ForceControl {
                target: name.clone(),
                role: ForceRole::Value,
                stored,
            },
        ));
        let select = varying.contains(&name).then(|| {
            let select = fresh("%force_select", serial, dimensions, signals);
            signals.push(Signal::new(select.clone(), bit_type(32)));
            (select, 0)
        });
        controls.insert(
            name,
            Controls {
                enable,
                value,
                select,
                stored,
            },
        );
    }
    let mut lowering = Lowering {
        controls: &mut controls,
        drivers: Vec::new(),
        process: 0,
    };
    for (index, process) in initial.iter_mut().enumerate() {
        lowering.process = index;
        lowering.block(&mut process.body);
    }
    let drivers = std::mem::take(&mut lowering.drivers);
    // One process for each target with a varying force: it waits for the
    // select, and keeps the value of the active statement.
    let mut by_target: Vec<(String, Vec<Driver>)> = Vec::new();
    for driver in drivers {
        match by_target
            .iter_mut()
            .find(|(target, _)| *target == driver.target)
        {
            Some((_, drivers)) => drivers.push(driver),
            None => by_target.push((driver.target.clone(), vec![driver])),
        }
    }
    for (target, drivers) in by_target {
        let controls = &controls[&target];
        let (select, _) = controls
            .select
            .as_ref()
            .expect("a varying force has a select");
        let mut body = vec![Stmt::WaitEvent(vec![any_change(Expr::Ident(
            select.clone(),
        ))])];
        let owner = drivers[0].process;
        for driver in drivers.into_iter().rev() {
            body = vec![Stmt::If {
                condition: select_is(select, driver.number),
                then_body: [assign(&controls.value, driver.rhs.clone())]
                    .into_iter()
                    .chain(controls.stored.then(|| Stmt::ForceStore {
                        lhs: LValue::Ident(target.clone()),
                        rhs: Expr::Ident(controls.value.clone()),
                    }))
                    .chain([Stmt::WaitEvent(vec![
                        any_change(Expr::Ident(select.clone())),
                        any_change(driver.rhs),
                    ])])
                    .collect(),
                else_body: body,
            }];
        }
        let owner = &initial[owner];
        let creation_order = owner.creation_order;
        let site = owner.site.clone().map(|mut site| {
            site.construct = "force";
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
                body,
            }],
            kind: celox_design::ProcessKind::Regular,
            validation_body: Vec::new(),
            synthetic: true,
            repeats: false,
        });
    }
    Ok(controls
        .into_iter()
        .map(|(target, controls)| (target, controls.enable))
        .collect())
}
