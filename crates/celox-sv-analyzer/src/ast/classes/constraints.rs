//! Scalar constraint binding. Unsupported forms fail before any RNG draws.
use super::*;
use celox_heap::randomization::{Expr as ConstraintExpr, Field, Program};
use celox_sv_syntax::{ExprId, ExprKind};

#[derive(Clone)]
struct Typed {
    kind: Kind,
    width: u32,
    signed: bool,
}
#[derive(Clone)]
enum Kind {
    Leaf(ConstraintExpr),
    Context(Box<Typed>),
    Unary(String, Box<Typed>),
    Binary(String, Box<Typed>, Box<Typed>),
}
fn relation(op: &str, left: Typed, right: Typed) -> Typed {
    Typed {
        kind: Kind::Binary(op.into(), Box::new(left), Box::new(right)),
        width: 1,
        signed: false,
    }
}

// Charge copies before allocating them: source nesting alone does not bound
// the expanded tree of nested membership predicates.
fn bounded_clone(value: &Typed, budget: &std::cell::Cell<usize>) -> Result<Typed, String> {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        charge(budget)?;
        match &value.kind {
            Kind::Leaf(_) => {}
            Kind::Context(child) | Kind::Unary(_, child) => pending.push(child),
            Kind::Binary(_, a, b) => pending.extend([a.as_ref(), b.as_ref()]),
        }
    }
    Ok(value.clone())
}

fn charge(budget: &std::cell::Cell<usize>) -> Result<(), String> {
    let remaining = budget
        .get()
        .checked_sub(1)
        .ok_or("constraint expression expansion limit")?;
    budget.set(remaining);
    Ok(())
}

fn sized(value: Typed, width: u32, signed: bool) -> Typed {
    Typed {
        kind: Kind::Context(Box::new(value)),
        width,
        signed,
    }
}

fn comparison(op: &str) -> bool {
    matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=" | "&&" | "||")
}

// Apply context sizing before arithmetic evaluation (IEEE 1800-2023 11.8.2).
// Keep operand types until this pass: eagerly extending signed children would
// incorrectly sign-extend them before a wider unsigned parent is encountered.
fn emit(value: Typed, context: Option<(u32, bool)>) -> ConstraintExpr {
    let (width, signed) = context.unwrap_or((value.width, value.signed));
    let (expr, result_width) = match value.kind {
        Kind::Leaf(expr) => (expr, value.width),
        Kind::Context(child) => (emit(*child, Some((value.width, value.signed))), value.width),
        Kind::Unary(op, child) => {
            let logical = op == "!";
            (
                ConstraintExpr::Unary {
                    op,
                    value: Box::new(emit(*child, (!logical).then_some((width, signed)))),
                },
                if logical { 1 } else { width },
            )
        }
        Kind::Binary(op, a, b) => {
            let is_comparison = comparison(&op);
            let logical = matches!(op.as_str(), "&&" | "||");
            let operands = if is_comparison {
                (a.width.max(b.width), a.signed && b.signed)
            } else {
                (width, signed)
            };
            let a = emit(*a, (!logical).then_some(operands));
            let b = emit(*b, (!logical).then_some(operands));
            (
                ConstraintExpr::Binary {
                    op,
                    left: Box::new(a),
                    right: Box::new(b),
                    signed: operands.1,
                },
                if is_comparison { 1 } else { width },
            )
        }
    };
    if result_width == width {
        expr
    } else {
        ConstraintExpr::Resize {
            value: Box::new(expr),
            width,
            signed,
        }
    }
}

pub(in crate::ast) fn bind(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    properties: &[Property],
    env: &HashMap<String, i128>,
) -> Result<ConstraintExpr, String> {
    bind_inputs(id, tree, properties, env, &HashMap::default())
}

pub(in crate::ast) fn bind_inputs(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    properties: &[Property],
    env: &HashMap<String, i128>,
    inputs: &HashMap<String, (usize, u32, bool)>,
) -> Result<ConstraintExpr, String> {
    fn expression(
        id: ExprId,
        tree: &AnalysisContext<'_>,
        properties: &[Property],
        env: &HashMap<String, i128>,
        inputs: &HashMap<String, (usize, u32, bool)>,
        depth: usize,
        budget: &std::cell::Cell<usize>,
    ) -> Result<Typed, String> {
        if depth > 64 {
            return Err("constraint expression nesting limit".into());
        }
        charge(budget)?;
        let recurse = |id| expression(id, tree, properties, env, inputs, depth + 1, budget);
        let native = tree.declarations();
        Ok(match &native.expression(id).kind {
            ExprKind::Group(id) => return recurse(*id),
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                if let Some((index, property)) = properties
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, p)| p.name == name)
                {
                    if property.field.four_state {
                        return Err("4-state class property in a constraint".into());
                    }
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Field(index)),
                        width: property.field.width,
                        signed: property.field.signed,
                    }
                } else if let Some(&(index, width, signed)) = inputs.get(&name) {
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Input { index, width }),
                        width,
                        signed,
                    }
                } else if env.contains_key(&name) {
                    // Parameter type reconstruction belongs to the general expression binder.
                    // Do not guess its width/sign here.
                    return Err("typed parameters in constraints".into());
                } else {
                    return Err(format!("constraint variable `{name}`"));
                }
            }
            ExprKind::Member {
                expression,
                separator,
                name,
            } if separator == "::"
                && matches!(&native.expression(*expression).kind, ExprKind::Name(root) if root.spelling == "local") =>
            {
                let key = format!("local::{}", instances::normalize_identifier(&name.spelling));
                let &(index, width, signed) = inputs.get(&key).ok_or("inline local variable")?;
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Input { index, width }),
                    width,
                    signed,
                }
            }
            ExprKind::Literal(text) => {
                if text.starts_with("'0") || text.starts_with("'1") {
                    return Err("unbased unsized constraint literal".into());
                }
                let literal =
                    typecheck::parse_integral_literal(text).ok_or("integral constraint literal")?;
                if literal.width > 64 || literal.mask != num_bigint::BigUint::default() {
                    return Err("constraint literal wider than 64 bits or containing X/Z".into());
                }
                let value = literal.value.to_u64_digits().first().copied().unwrap_or(0);
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Literal(literal.width as u32, value)),
                    width: literal.width as u32,
                    signed: literal.signed,
                }
            }
            ExprKind::Unary {
                operator,
                expression,
                postfix: false,
            } if matches!(operator.as_str(), "+" | "-" | "~" | "!") => {
                let value = recurse(*expression)?;
                if operator == "+" {
                    return Ok(value);
                }
                Typed {
                    width: if operator == "!" { 1 } else { value.width },
                    signed: operator != "!" && value.signed,
                    kind: Kind::Unary(operator.clone(), Box::new(value)),
                }
            }
            ExprKind::Binary {
                left,
                operator,
                right,
            } if operator == "inside" => {
                let value = recurse(*left)?;
                let ExprKind::Set(items) = &native.expression(*right).kind else {
                    return Err("constraint inside set".into());
                };
                // Determine the context across every member and range endpoint
                // before expanding comparisons, so arithmetic cannot truncate
                // differently depending on which alternative is being tested.
                let mut members = Vec::new();
                let mut width = value.width;
                let mut signed = value.signed;
                for item in items {
                    use celox_sv_syntax::DimensionKind;
                    let (low, high) = match item.kind {
                        DimensionKind::Index(id) => (recurse(id)?, None),
                        DimensionKind::Range { left, right } => {
                            (recurse(left)?, Some(recurse(right)?))
                        }
                        _ => return Err("constraint inside range".into()),
                    };
                    for operand in std::iter::once(&low).chain(high.iter()) {
                        width = width.max(operand.width);
                        signed &= operand.signed;
                    }
                    members.push((low, high));
                }
                let value = sized(value, width, signed);
                let mut alternatives = Vec::new();
                for (low, high) in members {
                    charge(budget)?;
                    let left = bounded_clone(&value, budget)?;
                    let low = sized(low, width, signed);
                    let predicate = if let Some(high) = high {
                        relation(
                            "&&",
                            relation(">=", left, low),
                            relation(
                                "<=",
                                bounded_clone(&value, budget)?,
                                sized(high, width, signed),
                            ),
                        )
                    } else {
                        relation("==", left, low)
                    };
                    alternatives.push(predicate);
                }
                // A balanced OR tree bounds recursion for large membership sets.
                while alternatives.len() > 1 {
                    let mut next = Vec::new();
                    let mut items = alternatives.into_iter();
                    while let Some(a) = items.next() {
                        next.push(match items.next() {
                            Some(b) => relation("||", a, b),
                            None => a,
                        });
                    }
                    alternatives = next;
                }
                alternatives.pop().ok_or("empty constraint inside set")?
            }
            ExprKind::Binary {
                left,
                operator,
                right,
            } if matches!(
                operator.as_str(),
                "+" | "-"
                    | "*"
                    | "&"
                    | "|"
                    | "^"
                    | "=="
                    | "!="
                    | "<"
                    | "<="
                    | ">"
                    | ">="
                    | "&&"
                    | "||"
            ) =>
            {
                let a = recurse(*left)?;
                let b = recurse(*right)?;
                let width = a.width.max(b.width);
                let signed = a.signed && b.signed;
                let logical = matches!(operator.as_str(), "&&" | "||");
                let comparison =
                    logical || matches!(operator.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=");
                Typed {
                    width: if comparison { 1 } else { width },
                    signed: !comparison && signed,
                    kind: Kind::Binary(operator.clone(), Box::new(a), Box::new(b)),
                }
            }
            _ => return Err("unsupported scalar constraint expression".into()),
        })
    }
    expression(
        id,
        tree,
        properties,
        env,
        inputs,
        0,
        &std::cell::Cell::new(16_384),
    )
    .map(|value| emit(value, None))
}

impl Class {
    pub fn constraint_program(&self) -> Converted<Program> {
        let mut fields = Vec::new();
        for property in &self.properties {
            if property.randc
                || property.field.handle.is_some()
                || property.field.width > 64
                || property.r#type.is_enum
            {
                return Err(unsupported(
                    "constraints currently require scalar non-enum properties without randc",
                ));
            }
            fields.push(Field {
                offset: property.field.offset,
                width: property.field.width,
                random: property.rand,
            });
        }
        let mut constraints = Vec::new();
        for (index, (_, block)) in self.constraints.iter().enumerate() {
            constraints.extend(
                block
                    .as_ref()
                    .map_err(|reason| unsupported(format!("constraints: {reason}")))?
                    .iter()
                    .cloned()
                    .map(|value| ConstraintExpr::Enabled {
                        index: index as u32,
                        value: Box::new(value),
                    }),
            );
        }
        Ok(Program {
            fields,
            constraints,
        })
    }
}

/// A generated call-specific method. Its immutable predicates never change the
/// object's persistent constraints, including during nested hook calls.
#[derive(Clone)]
pub(in crate::ast) struct Inline {
    pub key: usize,
    pub class: std::rc::Rc<Class>,
    pub constraints: Vec<ConstraintExpr>,
    pub inputs: Vec<Type>,
    pub dimensions: PackedDimensions,
}

impl Inline {
    pub fn prepare_name(&self, class: &str) -> String {
        format!("{}$prepare", self.name(class))
    }

    pub fn name(&self, class: &str) -> String {
        format!("{class}::$inline_randomize@{}", self.key)
    }
}

/// Bound inline predicates survive package imports without retaining syntax or
/// the declaring scope's dimensions. Consumer scopes can add dispatch targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ast) struct InlineRequest {
    pub key: usize,
    pub class: Class,
    pub constraints: Vec<ConstraintExpr>,
    pub inputs: Vec<Type>,
}

impl Inline {
    pub fn request(&self) -> InlineRequest {
        InlineRequest {
            key: self.key,
            class: (*self.class).clone(),
            constraints: self.constraints.clone(),
            inputs: self.inputs.clone(),
        }
    }
}
