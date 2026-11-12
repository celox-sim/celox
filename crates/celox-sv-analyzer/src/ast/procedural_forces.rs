//! Procedural force and release of variables and nets (IEEE 1800-2023
//! 10.6.2).
//!
//! `force t = e;` makes `t` take `e` until `release t;`, overriding every
//! procedural assignment, continuous assignment and port driver of `t`.
//! Each forced target `t` has two hidden variables that the frontend reads
//! wherever `t` is written: the enable mask `E`, and the forced value `V`.
//! Every write of `t` stores the bits of `V` where `E` is set instead of its
//! own (see [`crate::ir::ForceControl`]). A force or a release names all
//! bits `r` of `t`, or a constant select or member of it, and the
//! statements become
//!
//! ```text
//! force t[r] = e;  (statement k)   select_r = k or 0; V[r] = e; E[r] = '1; force-store t[r] = V[r];
//! release t[r];                    select_r = 0; E[r] = '0;
//! ```
//!
//! where the force-store puts the value in `t` at once and is no driver of
//! `t`. Only a target that procedures write, or an input port, is
//! force-stored: the writes of a continuous assignment or a port
//! connection depend on `E`, so they store `V` while it is set, and the
//! frontend gives a net that nothing else drives the driver `E ? V : 'z`
//! (see [`crate::ir::ForceControl::stored`]). A released variable keeps its
//! value until its next assignment; a released net, and a continuously
//! assigned variable, take the value of their drivers again. A target that
//! an `always_comb`, `always_latch` or `always @*` procedure assigns keeps
//! the forced value until the procedure executes again (9.2.2.2): the
//! release sets its hold `H |= E`, the writes of the target store `V` while
//! `E | H` is set, and one process for each such procedure waits on what
//! the procedure waits on and clears the holds of its targets. Parts of a
//! target that overlap are forced exactly only by constant expressions: an
//! overlap with a force of a varying expression is rejected. A target that a force
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

/// The bits of a target that a force or a release names: all of them, or
/// the constant range `(msb, lsb)` of a select or a member.
type Range = Option<(i128, i128)>;

/// The range `lhs` names (see [`Range`]); the binder made its bounds
/// literals (see `BodyBuilder::force_target`).
fn range(lhs: &LValue) -> Range {
    match lhs {
        LValue::Select { msb, lsb, .. } => {
            let bound = |bound: &ConstExpr| match bound {
                ConstExpr::Literal(text) => text.parse::<i128>().ok(),
                _ => None,
            };
            Some((bound(msb)?, bound(lsb)?))
        }
        _ => None,
    }
}

/// `lhs`, which names bits of a target, for the same bits of `name`.
fn renamed(lhs: &LValue, name: &str) -> LValue {
    let mut lhs = lhs.clone();
    match &mut lhs {
        LValue::Ident(target) | LValue::Select { name: target, .. } => *target = name.to_string(),
        LValue::Object { .. } => unreachable!("a force names a variable or a net"),
    }
    lhs
}

/// The same bits of the mask `name`, a two-state vector.
fn mask_part(lhs: &LValue, name: &str) -> LValue {
    match lhs {
        LValue::Select { msb, lsb, .. } => LValue::Select {
            name: name.to_string(),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: false,
            array_slice_width: None,
            array_slice_reversed: false,
            is_2state: false,
        },
        _ => LValue::Ident(name.to_string()),
    }
}

/// The value of the same bits of `name`.
fn read_part(lhs: &LValue, name: &str) -> Expr {
    match lhs {
        LValue::Select { msb, lsb, .. } => Expr::Select {
            expr: Box::new(Expr::Ident(name.to_string())),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: false,
        },
        _ => Expr::Ident(name.to_string()),
    }
}

/// The force controls of one target.
struct Controls {
    /// The mask of the forced bits.
    enable: String,
    value: String,
    /// For each range with a force of a varying expression, the select
    /// variable and the number of such forces seen so far.
    selects: Vec<(Range, String, usize)>,
    /// Whether a force stores its value into the target.
    stored: bool,
    /// The mask of the held bits of a target that procedures of the
    /// settle assign (see [`ForceRole::Hold`]).
    hold: Option<String>,
}

/// A force of a varying expression.
struct Driver {
    target: String,
    /// The bits it forces.
    lhs: LValue,
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
                    let bits = range(&lhs);
                    if let Some((_, select, count)) = controls
                        .selects
                        .iter_mut()
                        .find(|(selected, _, _)| *selected == bits)
                    {
                        let number = if varies(&rhs) {
                            *count += 1;
                            self.drivers.push(Driver {
                                target: target.clone(),
                                lhs: lhs.clone(),
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
                    lowered.push(Stmt::Assign {
                        lhs: renamed(&lhs, &controls.value),
                        rhs,
                        nonblocking: false,
                    });
                    lowered.push(Stmt::Assign {
                        lhs: mask_part(&lhs, &controls.enable),
                        rhs: literal("'1"),
                        nonblocking: false,
                    });
                    if controls.stored {
                        lowered.push(Stmt::ForceStore {
                            rhs: read_part(&lhs, &controls.value),
                            lhs,
                        });
                    }
                }
                Stmt::Release { lhs } => {
                    // A target nothing forces is released by nothing.
                    let Some(controls) = self.controls.get(lhs.name()) else {
                        continue;
                    };
                    let bits = range(&lhs);
                    if let Some((_, select, _)) = controls
                        .selects
                        .iter()
                        .find(|(selected, _, _)| *selected == bits)
                    {
                        lowered.push(assign(select, select_literal(0)));
                    }
                    // A released net, and a continuously assigned variable,
                    // take the value of their drivers at once (10.6.2):
                    // the writes of the drivers depend on the enable.
                    // A target a procedure of the settle assigns keeps the
                    // forced value until the procedure executes again.
                    if let Some(hold) = &controls.hold {
                        lowered.push(Stmt::Assign {
                            lhs: mask_part(&lhs, hold),
                            rhs: Expr::Binary {
                                left: Box::new(read_part(&lhs, hold)),
                                op: BinaryOp::BitOr,
                                right: Box::new(read_part(&lhs, &controls.enable)),
                            },
                            nonblocking: false,
                        });
                    }
                    lowered.push(Stmt::Assign {
                        lhs: mask_part(&lhs, &controls.enable),
                        rhs: literal("'0"),
                        nonblocking: false,
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

/// What the lowering does for a target.
struct Target {
    /// Whether a force stores its value into the target (see
    /// [`crate::ir::ForceControl::stored`]).
    stored: bool,
    /// The `always_comb`, `always_latch` and `always @*` procedures that
    /// assign the target, by position.
    holders: Vec<usize>,
}

/// Reject target `name` of a `force` or `release` that this lowering does
/// not support, and tell what the lowering does for it.
fn check_target(
    name: &str,
    dimensions: &PackedDimensions,
    ports: &[Port],
    signals: &[Signal],
    comb: &[CombProcess],
    ff: &[FfProcess],
    initial: &[InitialProcess],
) -> Converted<Target> {
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
    // release until the procedure executes again (9.2.2.2).
    let holders: Vec<usize> = comb
        .iter()
        .enumerate()
        .filter(|(_, process)| {
            process.kind != CombProcessKind::ContinuousAssign
                && (process
                    .assignments
                    .iter()
                    .any(|assignment| assignment.lhs.name() == name)
                    || writes(&process.body, name))
        })
        .map(|(index, _)| index)
        .collect();
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
    // A procedure of the settle need not write the target when it runs,
    // as a latch that is closed: a force stores into its targets, unless an
    // `always_comb` assigns the whole target each time it runs, as a
    // continuous assignment does (a store into storage the settle defines
    // loses its edges on the native backend, #3755).
    let assigned =
        comb.iter().any(|process| {
            let whole = |stmts: &[Stmt]| {
                fn whole(stmts: &[Stmt], name: &str) -> bool {
                    stmts.iter().any(|stmt| match stmt {
                        Stmt::Assign {
                            lhs: LValue::Ident(target),
                            ..
                        } => target == name,
                        Stmt::Block { body, .. } => whole(body, name),
                        _ => false,
                    })
                }
                whole(stmts, name)
            };
            match process.kind {
                CombProcessKind::ContinuousAssign => {
                    process
                        .assignments
                        .iter()
                        .any(|assignment| assignment.lhs.name() == name)
                        || writes(&process.body, name)
                }
                CombProcessKind::AlwaysComb => process.assignments.iter().any(
                    |assignment| matches!(&assignment.lhs, LValue::Ident(target) if target == name),
                ) || whole(&process.body),
                _ => false,
            }
        });
    Ok(Target {
        stored: input || (!net && !assigned),
        holders,
    })
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

/// The type of a mask of the bits of a target of type `target`, `width`
/// bits wide: a select of the target selects the same bits of it, by the
/// target's own packed ranges, or by offsets for a structure.
fn mask_type(target: &Type, width: usize) -> Type {
    if target.packed_ranges.is_empty() || !target.members.is_empty() {
        return bit_type(width);
    }
    let mut mask = Type::new(TypeKind::Bit);
    mask.packed_ranges = target.packed_ranges.clone();
    mask
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
    tree: &AnalysisContext<'_>,
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
    // The ranges each statement names, and whether a force of them has a
    // varying expression.
    let mut ranges: Vec<(String, Range, bool)> = Vec::new();
    let mut note = |lhs: &LValue, varying: bool| {
        if !names.iter().any(|name| name == lhs.name()) {
            names.push(lhs.name().to_string());
        }
        let bits = range(lhs);
        match ranges
            .iter_mut()
            .find(|(name, range, _)| name == lhs.name() && *range == bits)
        {
            Some((_, _, varies)) => *varies |= varying,
            None => ranges.push((lhs.name().to_string(), bits, varying)),
        }
    };
    for process in initial.iter() {
        for stmt in &process.body {
            stmt.walk(&mut |stmt| match stmt {
                Stmt::Force { lhs, rhs } => note(lhs, varies(rhs)),
                Stmt::Release { lhs } => note(lhs, false),
                _ => {}
            });
        }
    }
    if names.is_empty() {
        return Ok(HashMap::default());
    }
    let mut controls: HashMap<String, Controls> = HashMap::default();
    // The procedures of the settle that assign a target, with the holds of
    // their targets.
    let mut held: Vec<(usize, Vec<String>)> = Vec::new();
    for (serial, name) in names.into_iter().enumerate() {
        let Target { stored, holders } =
            check_target(&name, dimensions, ports, signals, comb, ff, initial)?;
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
        let width = dimensions
            .get(&name)
            .and_then(|shape| patterns::shape_width(&tree.functions, shape, dimensions))
            .ok_or_else(|| {
                unsupported(format!(
                    "force or release of `{name}`, whose width is not a constant"
                ))
            })?;
        // Two ranges of a target that overlap are forced by the forces of
        // each in turn, which only a constant expression keeps exact.
        let named: Vec<_> = ranges
            .iter()
            .filter(|(target, _, _)| *target == name)
            .map(|&(_, bits, varying)| {
                let (msb, lsb) = bits.unwrap_or((width as i128 - 1, 0));
                (bits, msb.max(lsb), msb.min(lsb), varying)
            })
            .collect();
        for (index, &(_, high, low, varying)) in named.iter().enumerate() {
            for &(_, other_high, other_low, other_varying) in &named[index + 1..] {
                if (varying || other_varying) && low <= other_high && other_low <= high {
                    return Err(unsupported(format!(
                        "force or release of overlapping parts of `{name}`, one of which is forced to a varying expression"
                    )));
                }
            }
        }
        let enable = fresh("%force_enable", serial, dimensions, signals);
        signals.push(Signal::force_control(
            enable.clone(),
            mask_type(&target_type, width),
            ForceControl {
                target: name.clone(),
                role: ForceRole::Enable,
                stored,
            },
        ));
        let value = fresh("%force_value", serial, dimensions, signals);
        signals.push(Signal::force_control(
            value.clone(),
            target_type.clone(),
            ForceControl {
                target: name.clone(),
                role: ForceRole::Value,
                stored,
            },
        ));
        let mut selects = Vec::new();
        for &(bits, _, _, varying) in &named {
            if varying {
                let select = fresh("%force_select", serial, dimensions, signals);
                signals.push(Signal::new(select.clone(), bit_type(32)));
                selects.push((bits, select, 0));
            }
        }
        let hold = (!holders.is_empty()).then(|| {
            let hold = fresh("%force_hold", serial, dimensions, signals);
            signals.push(Signal::force_control(
                hold.clone(),
                mask_type(&target_type, width),
                ForceControl {
                    target: name.clone(),
                    role: ForceRole::Hold,
                    stored,
                },
            ));
            for holder in holders {
                match held.iter_mut().find(|(process, _)| *process == holder) {
                    Some((_, holds)) => holds.push(hold.clone()),
                    None => held.push((holder, vec![hold.clone()])),
                }
            }
            hold
        });
        controls.insert(
            name,
            Controls {
                enable,
                value,
                selects,
                stored,
                hold,
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
    let mut by_target: Vec<(String, Range, Vec<Driver>)> = Vec::new();
    for driver in drivers {
        let bits = range(&driver.lhs);
        match by_target
            .iter_mut()
            .find(|(target, range, _)| *target == driver.target && *range == bits)
        {
            Some((_, _, drivers)) => drivers.push(driver),
            None => by_target.push((driver.target.clone(), bits, vec![driver])),
        }
    }
    for (target, bits, drivers) in by_target {
        let controls = &controls[&target];
        let (_, select, _) = controls
            .selects
            .iter()
            .find(|(selected, _, _)| *selected == bits)
            .expect("a varying force has a select");
        let mut body = vec![Stmt::WaitEvent(vec![any_change(Expr::Ident(
            select.clone(),
        ))])];
        let owner = drivers[0].process;
        for driver in drivers.into_iter().rev() {
            body = vec![Stmt::If {
                condition: select_is(select, driver.number),
                then_body: [Stmt::Assign {
                    lhs: renamed(&driver.lhs, &controls.value),
                    rhs: driver.rhs.clone(),
                    nonblocking: false,
                }]
                .into_iter()
                .chain(controls.stored.then(|| Stmt::ForceStore {
                    lhs: driver.lhs.clone(),
                    rhs: read_part(&driver.lhs, &controls.value),
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
    // One process for each such procedure: it waits on what the procedure
    // waits on, so it ends the holds when the procedure executes again.
    for (holder, holds) in held {
        let process = &comb[holder];
        let mut body = vec![Stmt::WaitImplicit {
            reads: process
                .body
                .iter()
                .chain(&process.sensitivity_body)
                .cloned()
                .collect(),
            always_comb: process.kind != CombProcessKind::AlwaysStar,
        }];
        body.extend(holds.iter().map(|hold| assign(hold, literal("'0"))));
        initial.push(InitialProcess {
            creation_order: process.creation_order,
            site: process.process_site.clone().map(|mut site| {
                site.construct = "release";
                site
            }),
            condition: process.condition.clone(),
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
