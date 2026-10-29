//! Tagged packed values and access checks (IEEE 1800-2023 7.3.2, 11.9).
use super::*;

pub(in crate::ast) struct Construction {
    tag: TaggedMember,
    pub shape: Option<VariableDimensions>,
    signed: bool,
}

pub(in crate::ast) fn construction_member(
    shape: &VariableDimensions,
    name: &str,
) -> Converted<Construction> {
    if !shape.members.first().is_some_and(PackedMember::is_tagged) {
        return Err(unsupported("tagged union expression for an untagged type"));
    }
    if !shape.unpacked.is_empty() || shape.packed.len() != 1 {
        return Err(unsupported(
            "tagged expression without a scalar union context",
        ));
    }
    let member = shape
        .members
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| unsupported(format!("unknown tagged union member `{name}`")))?;
    let tag = member
        .tag
        .clone()
        .ok_or_else(|| unsupported("tagged expression for an untagged type"))?;
    Ok(Construction {
        tag,
        shape: (!member.is_void).then(|| procedural::dimensions_from_type(&member.r#type)),
        signed: shape.signed,
    })
}

pub(in crate::ast) fn construct(
    state: &AnalysisState,
    plan: Construction,
    value: Option<Expr>,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    let mut parts = Vec::new();
    if plan.tag.tag_width > 0 {
        parts.push(Expr::Literal(format!(
            "{}'d{}",
            plan.tag.tag_width, plan.tag.index
        )));
    }
    let width = match &plan.shape {
        Some(shape) => patterns::shape_width(state, shape, dims)
            .ok_or_else(|| unsupported("tagged payload width"))?,
        None => 0,
    };
    if plan.tag.payload_width > width {
        parts.push(Expr::Literal(format!(
            "{}'bx",
            plan.tag.payload_width - width
        )));
    }
    match (plan.shape, value) {
        (Some(shape), Some(value)) => {
            let signed =
                dimensions::expression_signedness(&value, dims, &dims.const_env).unwrap_or(false);
            parts.push(patterns::assignment_value(
                state, &value, signed, None, &shape, dims, false,
            )?);
        }
        (None, None) => {}
        _ => {
            return Err(unsupported(
                "tagged member payload does not match void/non-void member",
            ));
        }
    }
    Ok(Expr::Resize {
        expr: Box::new(Expr::Concat(parts)),
        width: plan.tag.tag_width + plan.tag.payload_width,
        signed: plan.signed,
    })
}

/// A member path can cross several nested tags; each must be checked.
pub(in crate::ast) fn member_access(
    mut path: Vec<String>,
    dims: &PackedDimensions,
    state: &AnalysisState,
) -> Converted<Option<(Vec<Expr>, Type)>> {
    selects::bind_interface_path(state, &mut path, dims);
    let Some(root) = path.first().and_then(|name| dims.get(name)) else {
        return Ok(None);
    };
    if path.len() < 2 || !root.unpacked.is_empty() || root.packed.len() != 1 {
        return Ok(None);
    }
    let mut members = &root.members;
    let mut offset = 0;
    let mut guards = Vec::new();
    let mut result = None;
    for name in &path[1..] {
        let Some(member) = members.iter().find(|m| &m.name == name) else {
            return Ok(None);
        };
        if member.is_void {
            return Err(unsupported("access to a void tagged union member"));
        }
        if let Some(tag) = &member.tag {
            if tag.tag_width > 0 {
                let lsb = offset + tag.payload_width;
                guards.push(Expr::Binary {
                    left: Box::new(Expr::Select {
                        expr: Box::new(Expr::Ident(path[0].clone())),
                        msb: ConstExpr::Literal((lsb + tag.tag_width - 1).to_string()),
                        lsb: ConstExpr::Literal(lsb.to_string()),
                        signed: false,
                    }),
                    op: BinaryOp::EqCase,
                    right: Box::new(Expr::Literal(format!("{}'d{}", tag.tag_width, tag.index))),
                });
            }
        }
        offset += member.offset;
        result = Some(member.r#type.clone());
        members = &member.r#type.members;
    }
    Ok(result.map(|ty| (guards, ty)))
}

pub(in crate::ast) fn checked_value(
    guards: Vec<Expr>,
    mut value: Expr,
    state: &AnalysisState,
    dims: &PackedDimensions,
) -> Converted<Expr> {
    if guards.is_empty() {
        return Ok(value);
    }
    let width =
        expr_static_width(state, &value, dims).ok_or_else(|| unsupported("tagged access width"))?;
    let signed = dimensions::expression_signedness(&value, dims, &dims.const_env).unwrap_or(false);
    for guard in guards.into_iter().rev() {
        value = Expr::Call {
            callee: Callee::Checked {
                width,
                signed,
                check: crate::system_functions::AccessCheck::TaggedMember,
            },
            args: vec![Some(guard), Some(value)],
            arg_order: Vec::new(),
        };
    }
    Ok(value)
}

impl Type {
    pub(crate) fn tagged_identity(&self) -> Option<&str> {
        self.members
            .first()
            .filter(|member| member.is_tagged())
            .map(|member| member.owner.as_ref())
    }
}
