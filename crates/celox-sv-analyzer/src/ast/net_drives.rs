//! The implicit drivers of pull and supply nets (IEEE 1800-2023
//! 6.6.5-6). A supply net is driven constantly to its value. A pull
//! net takes its weak pull driver only when nothing else in the
//! module drives it: strength resolution against an authored driver
//! is not modeled, so a driven pull net reads Z where its driver
//! releases it instead of the pulled value.
use super::*;

pub(super) fn lower(
    ports: &[Port],
    instances: &[Instance],
    signals: &[Signal],
    processes: &mut Vec<CombProcess>,
) {
    for signal in signals {
        let (value, needs_undriven) = match signal.net_drive() {
            NetDrive::None => continue,
            NetDrive::Pull(value) => (value, true),
            NetDrive::Supply(value) => (value, false),
        };
        let name = signal.name();
        // A net that is also a port is driven across the hierarchy;
        // its implicit driver belongs to that resolution, which is not
        // modeled.
        if ports.iter().any(|port| port.name() == name) {
            continue;
        }
        if needs_undriven && (authored(processes, name) || connected(instances, name)) {
            continue;
        }
        processes.push(CombProcess::new(
            CombProcessKind::ContinuousAssign,
            None,
            vec![Assignment::new(
                LValue::Ident(name.to_string()),
                Expr::Literal(if value { "'1" } else { "'0" }.to_string()),
            )],
        ));
    }
}

/// Whether a continuous process of the module drives `name`.
fn authored(processes: &[CombProcess], name: &str) -> bool {
    fn writes(stmt: &Stmt, name: &str) -> bool {
        match stmt {
            Stmt::Assign { lhs, .. } => lhs.name() == name,
            Stmt::AssignConcat { parts, .. } => parts.iter().any(|lhs| lhs.name() == name),
            Stmt::If {
                then_body,
                else_body,
                ..
            } => {
                then_body.iter().any(|stmt| writes(stmt, name))
                    || else_body.iter().any(|stmt| writes(stmt, name))
            }
            _ => false,
        }
    }
    processes
        .iter()
        .filter(|process| process.kind == CombProcessKind::ContinuousAssign)
        .any(|process| {
            process
                .assignments
                .iter()
                .any(|assignment| assignment.lhs.name() == name)
                || process.body.iter().any(|stmt| writes(stmt, name))
        })
}

/// Whether an instance connection in the module references `name`.
fn connected(instances: &[Instance], name: &str) -> bool {
    fn mentions(expr: &Expr, name: &str) -> bool {
        match expr {
            Expr::Ident(ident) => ident == name,
            Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
                mentions(expr, name)
            }
            Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
                parts.iter().any(|part| mentions(part, name))
            }
            _ => false,
        }
    }
    instances
        .iter()
        .flat_map(|instance| &instance.port_connections)
        .any(|connection| {
            connection.actual() == name
                || connection
                    .actual_expr()
                    .is_some_and(|expr| mentions(expr, name))
        })
}
