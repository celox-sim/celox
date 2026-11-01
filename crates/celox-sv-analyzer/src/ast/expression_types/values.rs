//! Value expressions, without binding or evaluating their operands.
//! Width and signing are self-determined here (11.6.1, 11.8.1); assignment
//! context propagation remains in the existing binding path (11.8.2).
use super::*;
use semantic_types::Packed;

pub(super) fn resolve(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    root: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
) -> Option<TyId> {
    enum Job<'a> {
        Visit(ExprId),
        Unary(&'a str),
        Binary(&'a str),
        Conditional,
        Cast(Cast),
        Copy,
        Assignment,
    }
    let mut jobs = vec![Job::Visit(root)];
    let mut results = Vec::new();
    let mut constants = None;
    while let Some(job) = jobs.pop() {
        let ty = match job {
            Job::Visit(id) => {
                let id = ungroup(id, native);
                match &native.expression(id).kind {
                    ExprKind::Literal(text) => literal(&mut session.types, text)?,
                    ExprKind::Unary {
                        operator,
                        expression,
                        ..
                    } => {
                        jobs.push(Job::Unary(operator));
                        jobs.push(Job::Visit(*expression));
                        continue;
                    }
                    ExprKind::Binary {
                        left,
                        operator,
                        right,
                    } => {
                        jobs.push(Job::Binary(operator));
                        jobs.push(Job::Visit(*right));
                        jobs.push(Job::Visit(*left));
                        continue;
                    }
                    ExprKind::Conditional {
                        condition,
                        then_expression,
                        else_expression,
                    } => {
                        jobs.push(Job::Conditional);
                        jobs.push(Job::Visit(*else_expression));
                        jobs.push(Job::Visit(*then_expression));
                        jobs.push(Job::Visit(*condition));
                        continue;
                    }
                    ExprKind::Cast { target, expression } => {
                        let cast =
                            casting_type(tree, session, *target, native, dims, &mut constants)?;
                        jobs.push(Job::Cast(cast));
                        jobs.push(Job::Visit(*expression));
                        continue;
                    }
                    ExprKind::Copy(expression) => {
                        jobs.push(Job::Copy);
                        jobs.push(Job::Visit(*expression));
                        continue;
                    }
                    ExprKind::Assignment { left, right, .. } => {
                        jobs.push(Job::Assignment);
                        jobs.push(Job::Visit(*right));
                        jobs.push(Job::Visit(*left));
                        continue;
                    }
                    // Names, selections and calls use the same declaration
                    // query as other consumers. No value binding occurs here.
                    _ => type_of(tree, session, id, native, dims).ok()?,
                }
            }
            Job::Unary(operator) => {
                let operand = results.pop()?;
                unary(&mut session.types, operator, operand)?
            }
            Job::Binary(operator) => {
                let right = results.pop()?;
                let left = results.pop()?;
                binary(&mut session.types, operator, left, right)?
            }
            Job::Conditional => {
                let right = results.pop()?;
                let left = results.pop()?;
                // The predicate does not affect width, signing or real
                // promotion (11.3.1, 11.6.1), but an ambiguous predicate
                // can introduce X into an integral result (11.3.4, 11.4.11).
                let predicate = results.pop()?;
                conditional(&mut session.types, predicate, left, right)?
            }
            Job::Cast(cast) => cast.apply(&mut session.types, results.pop()?)?,
            Job::Copy => {
                let operand = results.pop()?;
                // A shallow copy has the source's static class type (8.12).
                matches!(session.types.data(operand), TyData::Class(_)).then_some(operand)?
            }
            Job::Assignment => {
                results.pop()?;
                // 11.3.6: an assignment expression has its left operand's
                // type. Ordinary binding checks the lvalue and compatibility.
                results.pop()?
            }
        };
        results.push(ty);
    }
    results.pop()
}

fn integral(
    table: &mut semantic_types::TyTable,
    width: u128,
    signed: bool,
    four_state: bool,
) -> Option<TyId> {
    let last = i128::try_from(width.checked_sub(1)?).ok()?;
    Some(
        table.intern(TyData::Packed(Packed {
            element: PackedElement::Bits,
            four_state,
            signed,
            signed_element_depth: None,
            dims: vec![Range {
                left: last,
                right: 0,
            }]
            .into(),
            keyword: None,
        })),
    )
}

fn literal(table: &mut semantic_types::TyTable, text: &str) -> Option<TyId> {
    if text == "null" {
        return Some(table.intern(TyData::Null));
    }
    // 5.7.1: unbased unsized literals are one bit in a self-determined
    // context. The value parser uses a wider fill value for later coercion.
    if matches!(text, "'0" | "'1" | "'x" | "'X" | "'z" | "'Z") {
        return integral(table, 1, false, true);
    }
    // 5.9: a string literal is an unsigned integer constant, not a string
    // variable. Decode with the binder's decoder, including triple quotes.
    if text.starts_with('"') {
        let bytes = expression_binding::string_literal_bytes(text)?;
        return integral(
            table,
            bytes.len().max(1).checked_mul(8)? as u128,
            false,
            true,
        );
    }
    // 5.7.1: integer constants are logic vectors. The existing metadata
    // parser reads a sized prefix without allocating its (possibly huge) value.
    if let Some((width, signed)) = typecheck::integral_literal_type_metadata(text) {
        return integral(table, width as u128, signed, true);
    }
    // 5.7.2: real literal constants have type real. Reject other primaries
    // (including time literals) until they have semantic types.
    if text.contains(['.', 'e', 'E']) && text.replace('_', "").parse::<f64>().is_ok() {
        return Some(table.intern(TyData::Real));
    }
    None
}

fn numeric(data: &TyData) -> bool {
    matches!(data, TyData::Packed(_) | TyData::Real | TyData::Shortreal)
}

fn four_state(data: &TyData) -> bool {
    matches!(data, TyData::Packed(packed) if packed.four_state)
}

fn unary(table: &mut semantic_types::TyTable, operator: &str, operand: TyId) -> Option<TyId> {
    let data = table.data(operand).clone();
    if !numeric(&data) {
        return None;
    }
    match (operator, data) {
        // 11.3.1: unary arithmetic preserves real/shortreal.
        ("+" | "-" | "++" | "--", TyData::Real | TyData::Shortreal) => Some(operand),
        ("!", data) => integral(table, 1, false, four_state(&data)),
        ("&" | "~&" | "|" | "~|" | "^" | "~^" | "^~", TyData::Packed(packed)) => {
            integral(table, 1, false, packed.four_state)
        }
        // 6.19.4: an enum in a numerical expression loses its enum identity.
        // Likewise arithmetic on a packed aggregate yields a vector.
        ("+" | "-" | "~" | "++" | "--", TyData::Packed(packed)) => {
            integral(table, packed.bits(), packed.signed, packed.four_state)
        }
        _ => None,
    }
}

fn binary(
    table: &mut semantic_types::TyTable,
    operator: &str,
    left: TyId,
    right: TyId,
) -> Option<TyId> {
    let left = table.data(left).clone();
    let right = table.data(right).clone();
    if !numeric(&left) || !numeric(&right) {
        return None;
    }
    let four_state = four_state(&left) || four_state(&right);
    // 11.3.1, 11.6.1: logical and relational results are one unsigned bit.
    if matches!(
        operator,
        "&&" | "||" | "->" | "<->" | "==" | "!=" | "<" | "<=" | ">" | ">="
    ) {
        return integral(table, 1, false, four_state);
    }
    // 11.3.1: only arithmetic operators support real operands; real takes
    // precedence over shortreal. Integer-only operators must not be guessed.
    if !matches!((&left, &right), (TyData::Packed(_), TyData::Packed(_))) {
        if !matches!(operator, "+" | "-" | "*" | "/" | "**") {
            return None;
        }
        return Some(table.intern(
            if matches!(left, TyData::Real) || matches!(right, TyData::Real) {
                TyData::Real
            } else {
                TyData::Shortreal
            },
        ));
    }
    let (TyData::Packed(left), TyData::Packed(right)) = (left, right) else {
        unreachable!()
    };
    match operator {
        // 11.4.6: case equality cannot yield x. Wildcard equality may.
        "===" | "!==" => integral(table, 1, false, false),
        "==?" | "!=?" => integral(table, 1, false, four_state),
        // 11.6.1: shift counts and exponents are self-determined, and do not
        // widen the result or change its signing.
        "<<" | ">>" | "<<<" | ">>>" | "**" => integral(
            table,
            left.bits(),
            left.signed,
            four_state || operator == "**",
        ),
        "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "^~" | "~^" => integral(
            table,
            left.bits().max(right.bits()),
            left.signed && right.signed,
            // 11.3.4: division/modulus can yield x even for 2-state operands.
            four_state || matches!(operator, "/" | "%"),
        ),
        _ => None,
    }
}

/// A cast's declared type or its transformation of an integral operand.
/// This query establishes a result type, not cast compatibility. The dedicated
/// cast relation and its validation belong to stage 6.
enum Cast {
    Declared(TyId),
    Size(u128),
    Signing(bool),
    Const,
}
impl Cast {
    fn apply(self, table: &mut semantic_types::TyTable, operand: TyId) -> Option<TyId> {
        match self {
            Self::Declared(target) => Some(target),
            Self::Const => Some(operand),
            Self::Size(_) | Self::Signing(_) => {
                let TyData::Packed(packed) = table.data(operand).clone() else {
                    return None;
                };
                let (width, signed) = match self {
                    Self::Size(width) => (width, packed.signed),
                    Self::Signing(signed) => (packed.bits(), signed),
                    _ => unreachable!(),
                };
                // 6.24.1: exactly one packed dimension, preserving the
                // operand's state domain, without enum/aggregate identity.
                integral(table, width, signed, packed.four_state)
            }
        }
    }
}

fn casting_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    target: ExprId,
    native: &Declarations,
    dims: &PackedDimensions,
    constants: &mut Option<Session>,
) -> Option<Cast> {
    let target = ungroup(target, native);
    if let ExprKind::Name(name) = &native.expression(target).kind {
        match name.spelling.as_str() {
            "const" => return Some(Cast::Const),
            "signed" => return Some(Cast::Signing(true)),
            "unsigned" => return Some(Cast::Signing(false)),
            _ => {}
        }
    }
    // Common literal sizes and predefined types require no constant-binding
    // session or type-table clone. Only dependent ranges/sizes need one.
    if let ExprKind::Literal(text) = &native.expression(target).kind
        && let Ok(size) = text.parse::<i128>()
    {
        return (size > 0).then_some(Cast::Size(u128::try_from(size).ok()?));
    }
    let ty = if let ExprKind::Type(id) = native.expression(target).kind {
        let data = native.data_type(id);
        if let celox_sv_syntax::DataTypeKind::Builtin(name) = &data.kind
            && data.packed_dimensions.is_empty()
        {
            type_binding::builtin_type(name, data.signed)?
        } else {
            let constants = constants.get_or_insert_with(|| constant_session(session));
            type_binding::binding::bind_in(id, native, tree, constants, dims).ok()??
        }
    } else if let Some(ty) = expression_binding::callable_name(tree, target, native)
        .and_then(|name| dims.type_aliases.get(&name))
    {
        ty.clone()
    } else {
        // The cast size is a constant, unlike the value being cast. Resolve
        // it in an isolated session, as for constant selection bounds.
        let constants = constants.get_or_insert_with(|| constant_session(session));
        let value = constant_binding::bind_in(target, native, tree, constants, dims).ok()??;
        let size = eval_ast_const_expr(&tree.functions, &value, &dims.const_env)?;
        return (size > 0).then_some(Cast::Size(u128::try_from(size).ok()?));
    };
    let id = semantic_types::of_type(&mut session.types, &tree.functions, &ty, &dims.const_env)?;
    Some(Cast::Declared(id))
}

fn conditional(
    table: &mut semantic_types::TyTable,
    predicate: TyId,
    left: TyId,
    right: TyId,
) -> Option<TyId> {
    let predicate_four_state = four_state(table.data(predicate));
    // 11.3.4, 11.4.11: differing branch bits merge to X for an ambiguous
    // predicate. Promote even matching two-state integral branch types.
    if predicate_four_state
        && let (TyData::Packed(a), TyData::Packed(b)) = (table.data(left), table.data(right))
        && (!a.four_state || !b.four_state)
    {
        let (width, signed) = (a.bits().max(b.bits()), a.signed && b.signed);
        return integral(table, width, signed, true);
    }
    // 11.4.11: matching branch types preserve named enums/aggregates, arrays
    // and handles; no value or constant-predicate folding chooses a branch.
    if table.matches(left, right) {
        return Some(left);
    }
    match (table.data(left).clone(), table.data(right).clone()) {
        (TyData::Packed(a), TyData::Packed(b)) => integral(
            table,
            a.bits().max(b.bits()),
            a.signed && b.signed,
            predicate_four_state || a.four_state || b.four_state,
        ),
        (TyData::Real, TyData::Packed(_) | TyData::Shortreal)
        | (TyData::Packed(_) | TyData::Shortreal, TyData::Real) => Some(table.intern(TyData::Real)),
        (TyData::Shortreal, TyData::Packed(_)) | (TyData::Packed(_), TyData::Shortreal) => {
            Some(table.intern(TyData::Shortreal))
        }
        // 11.4.11 b: null does not change the other branch's class type.
        (TyData::Null, TyData::Class(_)) => Some(right),
        (TyData::Class(_), TyData::Null) => Some(left),
        // Common class ancestors need the assignment relation (stage 5).
        // Other nonmatching nonintegral branches need contextual checking;
        // this singular-type query must not arbitrarily pick one branch.
        _ => None,
    }
}
