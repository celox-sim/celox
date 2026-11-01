//! Numeric value expressions, without binding or evaluating their operands.
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
    }
    let mut jobs = vec![Job::Visit(root)];
    let mut results = Vec::new();
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
    // (including null and time literals) until they have semantic types.
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
