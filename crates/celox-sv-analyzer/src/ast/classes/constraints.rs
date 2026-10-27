//! Scalar constraint binding. Unsupported forms fail before any RNG draws.
use super::*;
use celox_heap::randomization::{Expr as ConstraintExpr, Field, Program};
use celox_sv_syntax::{ExprId, ExprKind};

struct Typed {
    kind: Kind,
    width: u32,
    signed: bool,
}
enum Kind {
    Leaf(ConstraintExpr),
    Unary(String, Box<Typed>),
    Binary(String, Box<Typed>, Box<Typed>),
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

pub(super) fn bind(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    properties: &[Property],
    env: &HashMap<String, i128>,
) -> Result<ConstraintExpr, String> {
    fn expression(
        id: ExprId,
        tree: &AnalysisContext<'_>,
        properties: &[Property],
        env: &HashMap<String, i128>,
        depth: usize,
    ) -> Result<Typed, String> {
        if depth > 64 {
            return Err("constraint expression nesting limit".into());
        }
        let recurse = |id| expression(id, tree, properties, env, depth + 1);
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
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Field(index)),
                        width: property.field.width,
                        signed: property.field.signed,
                    }
                } else if env.contains_key(&name) {
                    // Parameter type reconstruction belongs to the general expression binder.
                    // Do not guess its width/sign here.
                    return Err("typed parameters in constraints".into());
                } else {
                    return Err(format!("constraint variable `{name}`"));
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
    expression(id, tree, properties, env, 0).map(|value| emit(value, None))
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
        for (_, block) in &self.constraints {
            constraints.extend(
                block
                    .as_ref()
                    .map_err(|reason| unsupported(format!("constraints: {reason}")))?
                    .clone(),
            );
        }
        Ok(Program {
            fields,
            constraints,
        })
    }
}
