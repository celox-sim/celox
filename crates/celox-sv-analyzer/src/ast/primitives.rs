//! Sequential user-defined primitives (IEEE 1800-2023 29.6–29.7).
//!
//! An instance of a sequential UDP keeps its state in a variable of the
//! scope, which a continuous assignment of the instance's delay drives its
//! output with (see `comb_process`), and the previous value of each input
//! in another, so that a change of an input is an edge of that input. A
//! process, each time an input changed (and once at its start, when the
//! inputs leave x), takes each input that changed, in the order of the
//! inputs, and sets the state to the next state of
//! the first level-sensitive table entry that matches, else of the first
//! edge-sensitive entry whose edge is that change, else to x (29.6.3,
//! 29.7). The state starts with the initial value of the UDP, or x (29.5).
use super::*;
use crate::procedural::{EventEdge, EventItemBase, LoopKind};
use celox_sv_syntax::{LogicGate, LogicGateInstance, Primitive};

/// The sequential UDP that the gate kind `kind` names, if any.
pub(super) fn sequential<'a>(
    native: &'a celox_sv_syntax::Declarations,
    kind: &str,
) -> Option<&'a Primitive> {
    native
        .primitives
        .iter()
        .find(|primitive| primitive.sequential && primitive.name.value() == kind)
}

/// The name of the instance, or of an unnamed one by where it is.
fn base(gate: &LogicGate, instance: &LogicGateInstance) -> String {
    // `%` starts no identifier of the source (see `continuous_delays`).
    match &instance.name {
        Some(name) => format!("%{}", instances::normalize_identifier(&name.spelling)),
        None => format!("%{}${}", gate.kind, instance.tokens.start),
    }
}

/// The variable that holds the state of `instance`.
pub(super) fn state_name(gate: &LogicGate, instance: &LogicGateInstance) -> String {
    format!("{}$state", base(gate, instance))
}

/// The variable that holds the previous value of input `input` of
/// `instance`.
fn previous_name(gate: &LogicGate, instance: &LogicGateInstance, input: usize) -> String {
    format!("{}$input{input}", base(gate, instance))
}

/// The variables of the instances `gate` of sequential UDP `primitive`,
/// or of the one `selected` names.
pub(super) fn signals(
    gate: &LogicGate,
    primitive: &Primitive,
    selected: Option<&str>,
    signals: &mut Vec<Signal>,
) {
    for instance in &gate.instances {
        let names = std::iter::once(state_name(gate, instance)).chain(
            (0..primitive.ports.len().saturating_sub(1))
                .map(|input| previous_name(gate, instance, input)),
        );
        for name in names {
            if selected.is_none_or(|selected| selected == name) {
                signals.push(Signal::new(name, Type::new(TypeKind::Logic)));
            }
        }
    }
}

fn is(value: &Expr, bit: &str) -> Expr {
    Expr::Binary {
        left: Box::new(value.clone()),
        op: BinaryOp::EqCase,
        right: Box::new(Expr::Literal(bit.into())),
    }
}

fn or(left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op: BinaryOp::LogicOr,
        right: Box::new(right),
    }
}

fn and(left: Expr, right: Expr) -> Expr {
    Expr::Binary {
        left: Box::new(left),
        op: BinaryOp::LogicAnd,
        right: Box::new(right),
    }
}

/// Whether `value` is neither 0 nor 1: x, or z, which a UDP takes as x
/// (29.3.5).
fn unknown(value: &Expr) -> Expr {
    Expr::Unary {
        op: UnaryOp::LogicNot,
        expr: Box::new(or(is(value, "1'b0"), is(value, "1'b1"))),
    }
}

/// Whether `value` matches level symbol `symbol` (Table 29-1); `None` for
/// `?`, which matches any value.
pub(super) fn level(symbol: &str, value: &Expr) -> Option<Expr> {
    match symbol {
        "0" => Some(is(value, "1'b0")),
        "1" => Some(is(value, "1'b1")),
        "b" => Some(or(is(value, "1'b0"), is(value, "1'b1"))),
        "x" => Some(unknown(value)),
        _ => None,
    }
}

/// Whether the change of an input from `previous` to `current` is the edge
/// `symbol` (Table 29-1).
fn edge(symbol: &str, previous: &Expr, current: &Expr) -> Expr {
    let changed = Expr::Binary {
        left: Box::new(previous.clone()),
        op: BinaryOp::NeCase,
        right: Box::new(current.clone()),
    };
    let levels = |from: &str, to: &str| {
        [
            level(from, previous),
            level(to, current),
            Some(changed.clone()),
        ]
        .into_iter()
        .flatten()
        .reduce(and)
        .expect("a change is a condition")
    };
    match symbol {
        "r" => levels("0", "1"),
        "f" => levels("1", "0"),
        "p" => or(or(levels("0", "1"), levels("0", "x")), levels("x", "1")),
        "n" => or(or(levels("1", "0"), levels("1", "x")), levels("x", "0")),
        "*" => changed,
        _ => {
            let symbols: Vec<char> = symbol
                .trim_start_matches('(')
                .trim_end_matches(')')
                .chars()
                .collect();
            levels(&symbols[0].to_string(), &symbols[1].to_string())
        }
    }
}

/// `value` as 0, 1 or x.
fn normalized(value: &Expr) -> Expr {
    Expr::Mux {
        condition: Box::new(is(value, "1'b0")),
        then_expr: Box::new(Expr::Literal("1'b0".into())),
        else_expr: Box::new(Expr::Mux {
            condition: Box::new(is(value, "1'b1")),
            then_expr: Box::new(Expr::Literal("1'b1".into())),
            else_expr: Box::new(Expr::Literal("1'bx".into())),
        }),
    }
}

fn is_level(symbol: &str) -> bool {
    matches!(symbol, "0" | "1" | "x" | "?" | "b")
}

/// The statements of the process of instance `instance` of sequential UDP
/// `primitive`, whose inputs are `inputs`.
pub(super) fn process(
    primitive: &Primitive,
    gate: &LogicGate,
    instance: &LogicGateInstance,
    inputs: Vec<Expr>,
    initial: Expr,
) -> Vec<Stmt> {
    let state = Expr::Ident(state_name(gate, instance));
    let previous: Vec<Expr> = (0..inputs.len())
        .map(|input| Expr::Ident(previous_name(gate, instance, input)))
        .collect();
    let assign = |name: String, rhs: Expr| Stmt::Assign {
        lhs: LValue::Ident(name),
        rhs,
        nonblocking: false,
    };
    let wait = Stmt::WaitEvent(
        inputs
            .iter()
            .map(|input| EventItemBase {
                edge: EventEdge::Any,
                expr: input.clone(),
                iff: None,
            })
            .collect(),
    );
    // A UDP takes z as x (29.3.5): the process compares and keeps inputs
    // as 0, 1 or x, so a change between z and x is none.
    let inputs: Vec<Expr> = inputs.iter().map(normalized).collect();
    let mut body = Vec::new();
    for (changed, input) in inputs.iter().enumerate() {
        // The inputs before this one have their new values, the ones after
        // it their old values: the changes take effect one at a time.
        let values: Vec<&Expr> = (0..inputs.len())
            .map(|other| {
                if other <= changed {
                    &inputs[other]
                } else {
                    &previous[other]
                }
            })
            .collect();
        let entries = primitive
            .rows
            .iter()
            .filter(|row| row.inputs.iter().all(|symbol| is_level(symbol)))
            .chain(
                primitive
                    .rows
                    .iter()
                    .filter(|row| !is_level(&row.inputs[changed])),
            );
        let mut next = Expr::Literal("1'bx".into());
        let entries: Vec<_> = entries.collect();
        for row in entries.into_iter().rev() {
            let condition = row
                .inputs
                .iter()
                .enumerate()
                .filter_map(|(position, symbol)| {
                    if is_level(symbol) {
                        level(symbol, values[position])
                    } else {
                        Some(edge(symbol, &previous[changed], input))
                    }
                })
                .chain(
                    row.state
                        .as_deref()
                        .and_then(|symbol| level(symbol, &state)),
                )
                .reduce(and);
            let value = match row.output.as_str() {
                "0" => Expr::Literal("1'b0".into()),
                "1" => Expr::Literal("1'b1".into()),
                "-" => state.clone(),
                _ => Expr::Literal("1'bx".into()),
            };
            next = match condition {
                Some(condition) => Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(value),
                    else_expr: Box::new(next),
                },
                None => value,
            };
        }
        body.push(Stmt::If {
            condition: Expr::Binary {
                left: Box::new(input.clone()),
                op: BinaryOp::NeCase,
                right: Box::new(previous[changed].clone()),
            },
            then_body: vec![
                assign(state_name(gate, instance), next),
                assign(previous_name(gate, instance, changed), input.clone()),
            ],
            else_body: Vec::new(),
        });
    }
    // The inputs start as x: the values they take before the process first
    // waits are changes it sees then.
    body.push(wait);
    let mut stmts = vec![assign(state_name(gate, instance), initial)];
    stmts.extend((0..inputs.len()).map(|input| {
        assign(
            previous_name(gate, instance, input),
            Expr::Literal("1'bx".into()),
        )
    }));
    stmts.push(Stmt::Loop {
        kind: LoopKind::Forever,
        init: Vec::new(),
        condition: None,
        step: Vec::new(),
        body,
    });
    stmts
}
