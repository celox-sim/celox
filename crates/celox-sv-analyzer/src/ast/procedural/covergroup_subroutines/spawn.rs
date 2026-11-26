//! `N::$spawn([this,] $cg)` of a covergroup type with a clocking event: what
//! samples an instance that a `new` constructs elsewhere than in the
//! initializer of a declaration of a module or a program (IEEE 1800-2023
//! 19.3; those have a sampler of their scope, see
//! `procedural::covergroup_sites`).
//!
//! For a type of a module or a program it adds the instance to the list of
//! the type, which the sampler of the type walks at the event
//! (`covergroup_sites::InstanceList`): no process is started. For a
//! covergroup embedded in a class, whose event names the object, and for a
//! type of a package or the compilation unit, whose scope has no sampler,
//! it starts one:
//!
//! ```text
//! function automatic T N::$spawn([this,] $cg);
//!     fork                                   // unseeded, join_none
//!         forever begin
//!             @(event);                      // bound where N is declared
//!             [if (this.cg != $cg) break;]   // embedded: a later `cg = new`
//!             N::$sample(this | $cg);
//!         end
//!     join_none
//!     return $cg;
//! endfunction
//! ```
//!
//! The child is a process of the analysis (`Stmt::Fork::unseeded`): it
//! draws no seed from the process that constructs the instance, so the
//! random values of that process stay those of the design without the
//! covergroup (18.14.1), and it is no child of that process, whose `wait
//! fork`, `disable fork` and `kill()` do not see it. It roots the instance,
//! and for an embedded covergroup the object, for the rest of the
//! simulation; an embedded one stops at the first event after the property
//! no longer holds the instance it samples, as a later `cg = new` of the
//! object makes it.

use super::*;

/// `N::$spawn` of `model`, whose clocking event is `event`.
pub(super) fn body(
    site: &Site<'_>,
    builder: &mut BodyBuilder<'_, '_, '_>,
    first: Option<String>,
    handle: &Type,
    event: celox_sv_syntax::TimingControlId,
) -> Converted<(String, Vec<Stmt>)> {
    let model = site.model;
    let mut cg = Expr::Ident(HANDLE.to_owned());
    builder.rename_expr(&mut cg);
    // The event is bound where the type is declared: in the scope that
    // declares it, or in the class of an embedded one, where a property
    // it names is one of `this`. The branch is a detached child, which
    // waits as an `initial` does.
    let body = std::mem::replace(&mut builder.body, system_functions::Body::Initial);
    let wait = builder.timing_control_statements(event);
    builder.body = body;
    // What the analysis does not support of the event is rejected as a
    // covergroup construct, which the tracking issue of covergroups keys on.
    let mut sampler = wait.map_err(|error| match error {
        AnalyzerError::Unsupported(text) if !text.starts_with("covergroup ") => {
            unsupported(format!("covergroup sampling event ({text})"))
        }
        error => error,
    })?;
    // An event on a value member of an object that a handle property
    // holds (`@(m_obj.m_v)`) reads two properties, which the event
    // expressions of processes do not support: a property of `this`
    // (`@m_z`) and a named event of such an object (`@(m_obj.m_ev)`) are.
    if reads_through_handle(&sampler) {
        return Err(unsupported(
            "covergroup sampling event on a member of an object that a handle property holds (IEEE 1800-2023 9.4.2, 19.3)",
        ));
    }
    let receiver = match (&first, site.class) {
        (Some(this), Some(class)) => {
            let property = class
                .properties
                .iter()
                .rev()
                .find(|property| {
                    Some(&property.owner) == model.owner.as_ref()
                        && property.name == model.identifier
                })
                .ok_or_else(|| {
                    unsupported(format!(
                        "covergroup `{}` has no property in its class",
                        model.display
                    ))
                })?;
            // A later `cg = new` of the object leaves this instance to
            // whatever else holds it, unsampled at the event, as the new
            // instance has a sampler of its own.
            sampler.push(Stmt::If {
                condition: Expr::Binary {
                    left: Box::new(expression_binding::objects::load(
                        property,
                        Expr::Ident(this.clone()),
                    )),
                    op: BinaryOp::Ne,
                    right: Box::new(cg.clone()),
                },
                then_body: vec![Stmt::Break],
                else_body: Vec::new(),
            });
            Expr::Ident(this.clone())
        }
        _ => cg.clone(),
    };
    sampler.push(Stmt::Call {
        name: model.sample_subroutine(),
        args: vec![Some(receiver)],
        arg_order: vec![0],
    });
    let result = builder.declare("$spawned", handle.clone());
    Ok((
        result.clone(),
        vec![
            Stmt::Fork {
                join: celox_design::ForkJoin::None,
                unseeded: true,
                branches: vec![vec![Stmt::Loop {
                    kind: LoopKind::Forever,
                    init: Vec::new(),
                    condition: None,
                    step: Vec::new(),
                    body: sampler,
                }]],
            },
            assign(&result, cg),
        ],
    ))
}

/// `N::$spawn($cg)` of a type whose instances `list` holds: it adds the
/// instance and returns it.
pub(super) fn listed(
    builder: &mut BodyBuilder<'_, '_, '_>,
    list: &crate::ast::procedural::covergroup_sites::InstanceList,
    handle: &Type,
) -> (String, Vec<Stmt>) {
    let mut cg = Expr::Ident(HANDLE.to_owned());
    builder.rename_expr(&mut cg);
    let result = builder.declare("$spawned", handle.clone());
    let mut body = list.add(&cg);
    body.push(assign(&result, cg));
    (result, body)
}

/// Whether an event control of `wait`, or its `iff`, reads a property of an
/// object through the read of another property.
fn reads_through_handle(wait: &[Stmt]) -> bool {
    use crate::ast::const_functions::{CallArguments, visit_expr_calls};
    let load =
        |callee: &Callee| matches!(callee, Callee::Object(op) if matches!(**op, ObjectOp::Load(_)));
    let mut found = false;
    for stmt in wait {
        let Stmt::WaitEvent(items) = stmt else {
            continue;
        };
        for expr in items
            .iter()
            .flat_map(|item| std::iter::once(&item.expr).chain(&item.iff))
        {
            visit_expr_calls(expr, &mut |callee, args| {
                if !load(callee) {
                    return;
                }
                let CallArguments::Expressions(args) = args else {
                    return;
                };
                for handle in args.iter().flatten() {
                    visit_expr_calls(handle, &mut |inner, _| found |= load(inner));
                }
            });
        }
    }
    found
}
