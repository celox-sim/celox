//! Scalar constraint binding. Unsupported forms fail before any RNG draws.
use super::*;
use celox_heap::randomization::{Array, Expr as ConstraintExpr, Field, Program};
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
/// Whether `value` reads a variable that `variable` tells: a field of the
/// class or a member of an object a handle property refers to.
fn typed_reads(value: &Typed, variable: &mut impl FnMut(&ConstraintExpr) -> bool) -> bool {
    match &value.kind {
        Kind::Leaf(expr) => reads(expr, variable),
        Kind::Context(child) | Kind::Unary(_, child) => typed_reads(child, variable),
        Kind::Binary(_, left, right) => typed_reads(left, variable) || typed_reads(right, variable),
    }
}

/// Whether `expr` reads a variable that `variable` tells.
fn reads(expr: &ConstraintExpr, variable: &mut impl FnMut(&ConstraintExpr) -> bool) -> bool {
    expr.any(&mut |expr| {
        matches!(
            expr,
            ConstraintExpr::Field(_) | ConstraintExpr::Member { .. }
        ) && variable(expr)
    })
}

/// Whether the member at byte offset `member` of the object a handle refers
/// to is randc (see `ConstraintExpr::Member`): the handle the static
/// property whose value is input `input` holds, else the handle property at
/// byte offset `handle`.
#[allow(clippy::too_many_arguments)]
fn randc_member(
    handle: u32,
    member: u32,
    input: Option<usize>,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
) -> Result<bool, String> {
    let class = match input {
        // A static handle, by the static property that input reads (see
        // `PathHandle::Static`).
        Some(input) => names.inputs.get(input).and_then(|input| {
            names
                .statics
                .iter()
                .find(|property| property.variable == input.variable)
                .and_then(|property| property.r#type.class_in(&session.types))
        }),
        None => properties
            .iter()
            .rev()
            .filter(|property| property.field.offset == handle)
            .find_map(|property| property.r#type.class_in(&session.types)),
    };
    let Some(class) = class else {
        return Ok(false);
    };
    let randc = |properties: &[Property]| {
        properties
            .iter()
            .any(|property| property.field.offset == member && property.randc)
    };
    // The class whose constraints are being bound has its properties here
    // (see `object_member`).
    if session
        .constraint_owners
        .last()
        .is_some_and(|(owner, _)| **owner == *class)
    {
        return Ok(randc(properties));
    }
    let class = super::class(tree, session, &class, env, &TypeAliases::default())
        .map_err(|_| format!("class `{class}`"))?;
    Ok(randc(&class.properties))
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
fn bounded_clone(value: &Typed, budget: &mut usize) -> Result<Typed, String> {
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

/// `items` joined by `op`, as a balanced tree, which bounds the depth of
/// recursion over large sets; `None` without items.
fn balanced(op: &str, mut items: Vec<Typed>) -> Option<Typed> {
    while items.len() > 1 {
        let mut next = Vec::with_capacity(items.len().div_ceil(2));
        let mut pending = items.into_iter();
        while let Some(a) = pending.next() {
            next.push(match pending.next() {
                Some(b) => relation(op, a, b),
                None => a,
            });
        }
        items = next;
    }
    items.pop()
}

fn charge(budget: &mut usize) -> Result<(), String> {
    *budget = budget
        .checked_sub(1)
        .ok_or("constraint expression expansion limit")?;
    Ok(())
}

fn sized(value: Typed, width: u32, signed: bool) -> Typed {
    Typed {
        kind: Kind::Context(Box::new(value)),
        width,
        signed,
    }
}

/// The value of constant expression `id`, if it is one.
fn constant(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &Session,
    env: &ConstEnv,
) -> Option<i128> {
    let mut constants = expression_types::constant_session(session);
    let dimensions = PackedDimensions::new(HashMap::default(), env, &TypeAliases::default());
    let value =
        constant_binding::bind_in(id, tree.declarations(), tree, &mut constants, &dimensions)
            .ok()??;
    constants::eval_ast_const_expr(&tree.functions, &value, env)
}

/// The value of integral constant expression `id` read as unsigned at the
/// width of its type, a constant function's result at that of the type
/// the function declares (13.4.3): its bits, of any width, as a weight
/// takes them (18.5.3).
fn constant_unsigned(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &Session,
    env: &ConstEnv,
) -> Option<num_bigint::BigUint> {
    let mut constants = expression_types::constant_session(session);
    let dimensions = PackedDimensions::new(HashMap::default(), env, &TypeAliases::default());
    let bound =
        constant_binding::bind_in(id, tree.declarations(), tree, &mut constants, &dimensions)
            .ok()??;
    let literal = if let ConstExpr::Function {
        callee: crate::system_functions::Callee::Subroutine(name),
        args,
        ..
    } = bound
    {
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        const_functions::eval_call_literal(&tree.functions, &name, &args, env)?
    } else {
        let bound =
            parameters::substitute_typed_parameter_literals_with_lookup(bound, env, &|name| {
                dimensions::parameter_type_from_const_env(env, name)
            });
        typecheck::eval_const_integral_literal_in_env(
            &bound.into(),
            &const_functions::Environment {
                constants: env,
                functions: &tree.functions,
            },
            &|_| None,
        )?
    };
    Some(literal.value)
}

/// A bound of a range of a distribution: an expression, or the value `$`
/// stands for.
#[derive(Clone, Copy)]
enum Bound {
    Expr(ExprId),
    Value(i128),
}

/// `value` as a value of a `width`-bit integral type of signedness `signed`.
fn in_type(value: i128, width: u32, signed: bool) -> i128 {
    if width == 0 || width >= 128 {
        return value;
    }
    let value = value & ((1i128 << width) - 1);
    if signed && value >> (width - 1) & 1 == 1 {
        value - (1i128 << width)
    } else {
        value
    }
}

fn shift(op: &str) -> bool {
    matches!(op, "<<" | ">>" | "<<<" | ">>>")
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
        Kind::Binary(op, a, b) if shift(&op) => {
            let a = emit(*a, Some((width, signed)));
            // The count is self-determined; the heap reads its value as
            // unsigned.
            let b = emit(*b, None);
            (
                ConstraintExpr::Binary {
                    op,
                    left: Box::new(a),
                    right: Box::new(b),
                    signed,
                },
                width,
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

/// Whether a constraint program holds property `property` as a field: an
/// integral property of at most 64 bits. Other properties, such as handles
/// and strings, are state a constraint does not name.
pub(in crate::ast) fn constrainable(property: &Property) -> bool {
    property.field.handle.is_none() && property.field.width <= 64
}

/// A constraint loop over the elements of a state array.
#[derive(Clone)]
struct Loop {
    variable: String,
    /// The array property, by its index.
    array: usize,
    element: std::sync::Arc<ContainerType>,
}

/// Property `member` of an object of class `class`, which a constraint
/// names through a handle, checked as a constraint variable: one of the
/// class whose constraints are being bound is among `properties`.
fn object_member(
    class: &str,
    member: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    env: &ConstEnv,
) -> Result<Property, String> {
    let owner = session.constraint_owners.last().cloned();
    let found = if owner.as_ref().is_some_and(|(owner, _)| *owner == class) {
        properties
            .iter()
            .rev()
            .find(|property| property.name == member)
            .cloned()
    } else if session
        .constraint_owners
        .iter()
        .any(|(owner, _)| *owner == class)
    {
        return Err(format!(
            "a constraint on objects of class `{class}` while its constraints are bound"
        ));
    } else {
        super::class(tree, session, class, env, &TypeAliases::default())
            .map_err(|_| format!("class `{class}`"))?
            .property(member)
            .cloned()
    };
    let property = found.ok_or_else(|| format!("constraint variable `{member}`"))?;
    visible(&property, tree, session, env)?;
    solver_holds(&property, tree, session, env)?;
    Ok(property)
}

/// The handle property a member path starts from (see [`handle_member`]).
enum PathHandle<'p> {
    /// An instance property of the class.
    Instance(&'p Property),
    /// A static property, a state value, by the input of the program that
    /// holds it (see `classes::StaticInput`).
    Static(usize),
}

/// The handle property and the member of the object it refers to that
/// constraint operand `id`, `h.m`, names (IEEE 1800-2023 18.5.9): `h`
/// resolves as every operand that names a property does (see
/// [`member_name`]), an instance or a static property alike, so `this.h.m`,
/// `super.h.m` and the receiver's `item.h.m` name one too.
#[allow(clippy::too_many_arguments)]
fn handle_member<'p>(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &'p [Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<(PathHandle<'p>, Property)>, String> {
    let native = tree.declarations();
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let Some((handle, lineage)) = member_name(*expression, native, names, inputs, loops)? else {
        return Ok(None);
    };
    let other = || "a member of a constraint operand other than an object";
    let (root, class) = match super::member(
        &handle,
        lineage,
        properties,
        names.aggregates,
        names.statics,
    ) {
        Some(super::Member::Instance(index)) => {
            let property = &properties[index];
            let class = property
                .r#type
                .class_in(&session.types)
                .ok_or_else(other)?
                .to_owned();
            (PathHandle::Instance(property), class)
        }
        Some(super::Member::Static(index)) => {
            let property = &names.statics[index];
            let class = property
                .r#type
                .class_in(&session.types)
                .ok_or_else(other)?
                .to_owned();
            accessible(
                &property.owner,
                &property.name,
                property.local,
                property.protected,
                tree,
                session,
                env,
            )?;
            let input = names
                .inputs
                .iter()
                .position(|input| input.variable == property.variable)
                .ok_or("a static class handle without its input")?;
            (PathHandle::Static(input), class)
        }
        _ => return Err(format!("constraint variable `{handle}`")),
    };
    let member = instances::normalize_identifier(&name.spelling);
    let property = object_member(&class, &member, tree, session, properties, env)?;
    Ok(Some((root, property)))
}

/// The loop over the array whose element `id`, `a[i]`, names, by its
/// level, with `i` the loop's variable.
fn loop_element(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    properties: &[Property],
    names: &Names<'_>,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<usize>, String> {
    let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(id).kind
    else {
        return Ok(None);
    };
    let Some((array, _)) = array_property(*expression, native, properties, names, inputs, loops)?
    else {
        return Ok(None);
    };
    if !loops.iter().any(|candidate| candidate.array == array) {
        return Ok(None);
    }
    // The loop whose variable the subscript is, which loops over the array.
    let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
        return Err("constraint array element other than its loop's".into());
    };
    let level = match &native.expression(index).kind {
        ExprKind::Name(name) => {
            let variable = instances::normalize_identifier(&name.spelling);
            loops
                .iter()
                .rposition(|candidate| candidate.variable == variable)
                .filter(|&level| loops[level].array == array)
        }
        _ => None,
    }
    .ok_or("constraint array element other than its loop's")?;
    Ok(Some(level))
}

/// The member `m` of the object loop element `a[i]` refers to that `id`,
/// `a[i].m`, names, with the loop's level.
#[allow(clippy::too_many_arguments)]
fn element_member(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<(usize, Property)>, String> {
    let native = tree.declarations();
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
    else {
        return Ok(None);
    };
    if separator != "." {
        return Ok(None);
    }
    let receiver = expression_binding::ungroup(*expression, native);
    let Some(level) = loop_element(receiver, native, properties, names, inputs, loops)? else {
        return Ok(None);
    };
    let class = loops[level]
        .element
        .element
        .class_in(&session.types)
        .ok_or("a member of a constraint array element other than an object")?
        .to_owned();
    let member = instances::normalize_identifier(&name.spelling);
    let property = object_member(&class, &member, tree, session, properties, env)?;
    Ok(Some((level, property)))
}

/// The element `a[i]`, or the member `a[i].m` of the object it refers to,
/// that `id` names, with `i` the variable of the loop over `a`.
#[allow(clippy::too_many_arguments)]
fn element(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<Typed>, String> {
    if loops.is_empty() {
        return Ok(None);
    }
    let native = tree.declarations();
    let (level, member, width, signed) =
        match element_member(id, tree, session, properties, names, env, inputs, loops)? {
            Some((level, property)) => (
                level,
                Some(property.field.offset),
                property.field.width,
                property.field.signed,
            ),
            None => {
                let Some(level) = loop_element(id, native, properties, names, inputs, loops)?
                else {
                    return Ok(None);
                };
                let element = &loops[level].element;
                if !solver_element(element, tree, session, env) {
                    return Err("a constraint array element other than 2-state integral".into());
                }
                (level, None, element.layout.width, element.layout.signed)
            }
        };
    Ok(Some(Typed {
        kind: Kind::Leaf(ConstraintExpr::Element {
            level: level as u32,
            member,
            width,
        }),
        width,
        signed,
    }))
}

/// The element `a[c]` that `id` names, of an array property `a` of the
/// class at constant position `c`, outside a loop over it or at another
/// position than the loop's (IEEE 1800-2023 18.4, 18.5.8): a solver
/// variable of a random array, else its value.
#[allow(clippy::too_many_arguments)]
fn element_at(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<Typed>, String> {
    let native = tree.declarations();
    let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(id).kind
    else {
        return Ok(None);
    };
    let celox_sv_syntax::DimensionKind::Index(index) = &dimension.kind else {
        return Ok(None);
    };
    let Some((position, property)) =
        array_property(*expression, native, properties, names, inputs, loops)?
    else {
        return Ok(None);
    };
    // A loop variable or another value the solver chooses is no constant.
    let Some(index) = constant(*index, tree, session, env) else {
        return Ok(None);
    };
    if (property.rand || property.randc) && !random_array(property, tree, session, env) {
        return Err(
            "a constraint on an element of a rand array of elements other than 2-state integral ones of at most 64 bits"
                .into(),
        );
    }
    // The positions of a fixed-size array are its indices only from 0.
    if property.storage.as_ref().is_some_and(|storage| {
        storage
            .fixed
            .is_some_and(|(left, right)| left != 0 || left > right)
    }) {
        return Err(
            "a constraint on an element of a fixed-size array whose range is not [0:N-1]".into(),
        );
    }
    visible(property, tree, session, env)?;
    let element = property.array_storage().expect("an array property").clone();
    if !solver_element(&element, tree, session, env) {
        return Err("a constraint array element other than 2-state integral".into());
    }
    let array = properties[..position]
        .iter()
        .filter(|property| property.constraint_array())
        .count();
    let (width, signed) = (element.layout.width, element.layout.signed);
    Ok(Some(Typed {
        kind: Kind::Leaf(ConstraintExpr::ElementAt {
            array,
            // A negative position is past the end too.
            index: u64::try_from(index).unwrap_or(u64::MAX),
            width,
        }),
        width,
        signed,
    }))
}

/// The semantic type of constraint operand `id` whose bits a select reads,
/// as the shared type computation gives it: of the instance or static
/// property of the class it names (see [`member_name`]), or of an element
/// of an unpacked array it selects, as `a[0]` (see
/// [`expression_types::select`]).
#[allow(clippy::too_many_arguments)]
/// Whether `id` is a part-select, which no select follows (IEEE 1800-2023
/// A.8.4: bit-selects come first, and one part-select may close them).
fn part_selected(id: ExprId, native: &celox_sv_syntax::Declarations) -> bool {
    matches!(
        &native.expression(expression_binding::ungroup(id, native)).kind,
        ExprKind::Select { dimension, .. }
            if !matches!(dimension.kind, celox_sv_syntax::DimensionKind::Index(_))
    )
}

fn select_operand_type(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Option<semantic_types::TyId> {
    let native = tree.declarations();
    let id = expression_binding::ungroup(id, native);
    if let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(id).kind
    {
        let base = select_operand_type(
            *expression,
            tree,
            session,
            properties,
            names,
            env,
            inputs,
            loops,
        )?;
        // An element of an unpacked array, or of a packed array of several
        // dimensions, whose selects are those of its element type (IEEE
        // 1800-2023 7.4.5). A bit of a vector selects no further.
        if part_selected(*expression, native) {
            return None;
        }
        let packed_array = semantic_types::packed_bounds_of(&session.types, base)
            .is_some_and(|bounds| bounds.len() > 1);
        if !packed_array
            && !matches!(
                session.types.data(base),
                semantic_types::TyData::Array { .. }
            )
        {
            return None;
        }
        let dimensions = PackedDimensions::new(HashMap::default(), env, &TypeAliases::default());
        return expression_types::select(tree, session, base, &dimension.kind, native, &dimensions);
    }
    // A member of the object a handle or a loop element refers to.
    let member = match handle_member(id, tree, session, properties, names, env, inputs, loops) {
        Ok(Some((_, member))) => Some(member),
        _ => element_member(id, tree, session, properties, names, env, inputs, loops)
            .ok()
            .flatten()
            .map(|(_, member)| member),
    };
    if let Some(member) = member {
        return semantic_types::of_type(&mut session.types, &tree.functions, &member.r#type, env);
    }
    let (name, lineage) = member_name(id, native, names, inputs, loops).ok()??;
    let ty = match super::member(&name, lineage, properties, names.aggregates, names.statics)? {
        super::Member::Instance(index) => properties[index].r#type.clone(),
        super::Member::Static(index) => names.statics[index].r#type.clone(),
        // A member of an unpacked structure property has no select here.
        super::Member::Aggregate(_) => return None,
    };
    semantic_types::of_type(&mut session.types, &tree.functions, &ty, env)
}

/// The lowest bit and the width of the bits that select `dimension` takes
/// of a `width`-bit value of type `ty`, at constant positions of its
/// outermost packed dimension (IEEE 1800-2023 7.4.6, 11.5.1), and whether
/// they are signed, by the type of the select: an element of a signed
/// packed element type is (7.4.1), a bit-select or part-select is not
/// (11.8.1).
fn selected_bits(
    dimension: &celox_sv_syntax::Dimension,
    base: semantic_types::TyId,
    width: u32,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(u32, u32, bool), String> {
    use celox_sv_syntax::DimensionKind;
    let bounds = semantic_types::packed_bounds_of(&session.types, base)
        .ok_or("a select of a constraint variable of no packed type")?;
    // An integral type without packed dimensions, such as `int` or a packed
    // structure, selects as if declared `[width-1:0]` (6.11.1, 7.2.1).
    let (left, right) = bounds
        .first()
        .copied()
        .unwrap_or((i128::from(width) - 1, 0));
    let size = left.abs_diff(right) + 1;
    if size == 0 || u128::from(width) % size != 0 {
        return Err("a select of a constraint variable of unknown layout".into());
    }
    let element = u128::from(width) / size;
    let descending = left >= right;
    // The offset of position `index` from the lowest one.
    let offset = |index: i128| -> Result<u128, String> {
        if index < left.min(right) || index > left.max(right) {
            return Err("a constraint select outside the range of its variable".into());
        }
        Ok(if descending {
            index.abs_diff(right)
        } else {
            right.abs_diff(index)
        })
    };
    let value = |id: ExprId| {
        constant(id, tree, session, env).ok_or("a constraint select at a non-constant position")
    };
    let (low, high) = match dimension.kind {
        DimensionKind::Index(index) => {
            let index = value(index)?;
            (index, index)
        }
        DimensionKind::Range {
            left: msb,
            right: lsb,
        } => {
            let (msb, lsb) = (value(msb)?, value(lsb)?);
            // A part-select runs in the direction of the range (11.5.1).
            if (msb >= lsb) != descending && msb != lsb {
                return Err("a constraint part-select against the direction of its range".into());
            }
            (msb, lsb)
        }
        DimensionKind::Indexed {
            base,
            ref operator,
            width: count,
        } => {
            let (base, count) = (value(base)?, value(count)?);
            if count <= 0 {
                return Err("a constraint indexed part-select of no bits".into());
            }
            // An endpoint past the integers is outside the range too.
            let outside = || "a constraint select outside the range of its variable".to_owned();
            if operator == "+:" {
                (base, base.checked_add(count - 1).ok_or_else(outside)?)
            } else {
                (base.checked_sub(count - 1).ok_or_else(outside)?, base)
            }
        }
        _ => return Err("unsupported constraint select".into()),
    };
    let (a, b) = (offset(low)?, offset(high)?);
    let lowest = a.min(b) * element;
    let bits = (a.abs_diff(b) + 1) * element;
    let dimensions = PackedDimensions::new(HashMap::default(), env, &TypeAliases::default());
    let selected = expression_types::select(
        tree,
        session,
        base,
        &dimension.kind,
        tree.declarations(),
        &dimensions,
    )
    .ok_or("a select of a constraint variable of no type")?;
    let signed = matches!(
        session.types.data(selected),
        semantic_types::TyData::Packed(packed) if packed.signed
    );
    Ok((lowest as u32, bits as u32, signed))
}

/// Whether the constraint being bound may access property `property`: a
/// local member in its class only, a protected one also in those deriving
/// from it (IEEE 1800-2023 8.18), from the constraint's lexical class.
fn visible(
    property: &Property,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    accessible(
        &property.owner,
        &property.name,
        property.local,
        property.protected,
        tree,
        session,
        env,
    )
}

/// Whether the constraint being bound may access member `name` of class
/// `owner`, `local` or `protected` (see [`visible`]).
fn accessible(
    owner: &str,
    name: &str,
    local: bool,
    protected: bool,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    if !local && !protected {
        return Ok(());
    }
    let scope = session.constraint_scopes.last().cloned();
    let visible = match &scope {
        Some((scope, _)) if scope == owner => true,
        Some((_, Some(base))) if !local => {
            super::class(tree, session, base, env, &TypeAliases::default()).is_ok_and(|base| {
                base.derives_from(tree, session, owner, env, &TypeAliases::default())
            })
        }
        _ => false,
    };
    if visible {
        Ok(())
    } else {
        Err(format!(
            "{} member `{name}` of class `{owner}`",
            if local { "local" } else { "protected" },
        ))
    }
}

/// The names a constraint of a class may name besides its properties: its
/// static properties, those the solver reads as inputs of the program and
/// the rand ones it solves in the object's slots, and the class's lineage,
/// which tells which of them a name denotes (see `classes::member`).
pub(in crate::ast) struct Names<'a> {
    pub lineage: &'a [String],
    /// The identifier that declares each class of the lineage (see
    /// `classes::scope_identifiers`), by which a class scope names it.
    pub scopes: &'a [String],
    pub aggregates: &'a [super::AggregateProperty],
    pub statics: &'a [super::StaticProperty],
    pub inputs: &'a [super::StaticInput],
    pub slots: &'a [super::StaticSlot],
    /// The caller's variable through which inline constraints name the
    /// object their call randomizes, `h` of `h.randomize() with {...}`,
    /// when its name resolves in the caller's scope (IEEE 1800-2023 18.7):
    /// read once as the call starts, it holds that object, so a dotted name
    /// it begins names a member of the object as `this` does, also after a
    /// `pre_randomize()` that assigns the variable.
    pub receiver: Option<&'a str>,
}

/// The instance property that `name` denotes, by its index, if a member of
/// the class.
fn instance<'p>(
    name: &str,
    lineage: &[String],
    properties: &'p [Property],
    names: &Names<'_>,
) -> Option<(usize, &'p Property)> {
    match super::member(name, lineage, properties, names.aggregates, names.statics)? {
        super::Member::Instance(index) => Some((index, &properties[index])),
        super::Member::Aggregate(_) | super::Member::Static(_) => None,
    }
}

/// The member of the class that constraint operand `id` names, with the
/// lineage of the class to look it up in, from the class on, as every
/// operand that names a property resolves it: a name other than a variable
/// of a loop around it or of the caller (IEEE 1800-2023 18.7), or one that
/// `this` or `super` qualifies, which names a member of the class or of its
/// base (8.11, 8.15).
fn member_name<'n>(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    names: &Names<'n>,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<(String, &'n [String])>, String> {
    match &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            let local =
                inputs.contains_key(&name) || loops.iter().any(|looped| looped.variable == name);
            Ok((!local).then_some((name, names.lineage)))
        }
        // A class scope of the class or a base names a member of it, such
        // as a static property (IEEE 1800-2023 8.23).
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => {
            let ExprKind::Name(root) = &native
                .expression(expression_binding::ungroup(*expression, native))
                .kind
            else {
                return Ok(None);
            };
            let root = instances::normalize_identifier(&root.spelling);
            Ok(names
                .lineage
                .iter()
                .zip(names.scopes.iter().map(Some).chain(std::iter::repeat(None)))
                .position(|(class, scope)| *class == root || scope == Some(&root))
                .map(|position| {
                    (
                        instances::normalize_identifier(&name.spelling),
                        &names.lineage[position..],
                    )
                }))
        }
        // A path the caller supplies, as `this.x` or `super.x` in a scope
        // randomize call inside a method (see [`caller_key`]), is no member
        // of the class.
        ExprKind::Member { .. }
            if caller_key(expression_binding::ungroup(id, native), native, true)
                .is_some_and(|key| inputs.contains_key(&key)) =>
        {
            Ok(None)
        }
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." => {
            let ExprKind::Name(root) = &native
                .expression(expression_binding::ungroup(*expression, native))
                .kind
            else {
                return Ok(None);
            };
            let lineage = match root.spelling.as_str() {
                "this" => names.lineage,
                "super" => names
                    .lineage
                    .get(1..)
                    .filter(|base| !base.is_empty())
                    .ok_or("`super` in a class without a base class")?,
                root if names.receiver == Some(instances::normalize_identifier(root).as_str())
                    && !loops
                        .iter()
                        .any(|looped| looped.variable == instances::normalize_identifier(root)) =>
                {
                    names.lineage
                }
                _ => return Ok(None),
            };
            Ok(Some((
                instances::normalize_identifier(&name.spelling),
                lineage,
            )))
        }
        _ => Ok(None),
    }
}

/// Name static rand property `n` of constraint `expr` as field `first + n`
/// of its program (see `ConstraintExpr::Static`).
pub(in crate::ast) fn resolve_statics(expr: &mut ConstraintExpr, first: usize) {
    expr.rewrite(&mut |expr| {
        if let ConstraintExpr::Static(slot) = expr {
            *expr = ConstraintExpr::Field(first + *slot);
        }
    });
}

/// A value of the caller that a randomize call supplies to its inline
/// constraints (IEEE 1800-2023 18.7), input `index` of the program, or a
/// variable of a scope randomize call (18.12) with an index from
/// [`SCOPE_FIELDS`] on: `width` bits, `signed`, of semantic type `ty` when
/// known.
#[derive(Clone, Copy, Debug)]
pub(in crate::ast) struct CallerInput {
    pub index: usize,
    pub width: u32,
    pub signed: bool,
    pub ty: Option<semantic_types::TyId>,
}

/// The key of the caller's value that constraint expression `id` names, as
/// a randomize call supplies it (see [`CallerInput`]): a name, `local::`
/// one (18.7), with `this_is_caller`, as in a scope randomize call inside a
/// method, `this.` or `super.` one, a member of the caller's object, or an
/// element `a[i]` of an array at a position a name or a literal gives,
/// which the call supplies when both resolve in the caller's scope. One
/// resolution for the call's collection and the binding.
pub(in crate::ast) fn caller_key(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    this_is_caller: bool,
) -> Option<String> {
    match &native.expression(id).kind {
        ExprKind::Name(name) if name.spelling != "$" => {
            Some(instances::normalize_identifier(&name.spelling))
        }
        ExprKind::Member {
            expression,
            separator,
            name,
        } => {
            let ExprKind::Name(root) = &native
                .expression(expression_binding::ungroup(*expression, native))
                .kind
            else {
                return None;
            };
            let name = instances::normalize_identifier(&name.spelling);
            match (separator.as_str(), root.spelling.as_str()) {
                ("::", "local") => Some(format!("local::{name}")),
                (".", root @ ("this" | "super")) if this_is_caller => {
                    Some(format!("{root}.{name}"))
                }
                _ => None,
            }
        }
        ExprKind::Select {
            expression,
            dimension,
        } => {
            let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
                return None;
            };
            let ExprKind::Name(array) = &native
                .expression(expression_binding::ungroup(*expression, native))
                .kind
            else {
                return None;
            };
            let index = match &native
                .expression(expression_binding::ungroup(index, native))
                .kind
            {
                ExprKind::Name(name) if name.spelling != "$" => {
                    instances::normalize_identifier(&name.spelling)
                }
                ExprKind::Literal(text) => text.clone(),
                _ => return None,
            };
            Some(format!(
                "{}[{index}]",
                instances::normalize_identifier(&array.spelling)
            ))
        }
        _ => None,
    }
}

/// The key of the caller's value that constraint expression `id` computes
/// as a whole, as a randomize call supplies it when the expression reads no
/// random variable (see `objects::caller_expression`): its source range.
pub(in crate::ast) fn expression_key(id: ExprId, native: &celox_sv_syntax::Declarations) -> String {
    let tokens = native.expression(id).tokens;
    format!("({}..{})", tokens.start, tokens.end)
}

/// The names of the array and the position of element `a[i]` that
/// constraint expression `id` selects (see [`caller_key`]), the position's
/// only when a name gives it.
pub(in crate::ast) fn element_names(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Option<(String, Option<String>)> {
    let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(id).kind
    else {
        return None;
    };
    let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
        return None;
    };
    let ExprKind::Name(array) = &native
        .expression(expression_binding::ungroup(*expression, native))
        .kind
    else {
        return None;
    };
    let index = match &native
        .expression(expression_binding::ungroup(index, native))
        .kind
    {
        ExprKind::Name(name) => Some(instances::normalize_identifier(&name.spelling)),
        _ => None,
    };
    Some((instances::normalize_identifier(&array.spelling), index))
}

/// The first input index that stands for the value of call `k` of a
/// function the constraints of a class make (see `Class::constraint_calls`),
/// which its randomize call places after the inputs it has.
pub(in crate::ast) const RESULT_INPUTS: usize = 1 << 28;

/// Record the call of function `callee` of the class whose constraints are
/// being bound, with `arguments` (IEEE 1800-2023 18.5.11): its index among
/// the class's calls, and the width and signedness of its value. A function
/// that is not of the class, with an argument other than an input, or in
/// inline constraints is unsupported.
#[allow(clippy::too_many_arguments)]
fn function_call(
    callee: ExprId,
    actuals: &[celox_sv_syntax::Argument],
    bind: &mut dyn FnMut(&mut Session, ExprId) -> Result<Typed, String>,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<(usize, u32, bool), String> {
    use celox_sv_syntax::Direction;
    let native = tree.declarations();
    // A method of the class, named as other constraint operands name a
    // member (see [`member_name`]): one qualified by `super`, or by a class
    // scope of a base, is the one that class sees, called as such (IEEE
    // 1800-2023 8.15, 8.23).
    let (name, lineage) = member_name(callee, native, names, inputs, loops)?
        .ok_or("a call in a constraint other than of a function of the class")?;
    let Some((methods, _, block)) = session.constraint_methods.last() else {
        return Err(format!("a call of `{name}` in inline constraints"));
    };
    let block = *block;
    let method = if lineage.len() < names.lineage.len() {
        let base = lineage[0].clone();
        super::class(tree, session, &base, env, &TypeAliases::default())
            .map_err(|_| format!("class `{base}`"))?
            .method(&name)
            .cloned()
            .map(|mut method| {
                method.is_virtual = false;
                method
            })
    } else {
        methods.iter().find(|method| method.name == name).cloned()
    }
    .ok_or_else(|| format!("a call of `{name}` in a constraint"))?;
    let unsupported = |what: &str| format!("function `{name}` in a constraint {what}");
    if !method.is_function || method.qualifiers.is_static {
        return Err(unsupported("that is not a function of the object"));
    }
    let Some(Some(result)) = &method.result else {
        return Err(unsupported("without a value"));
    };
    let four_state = !matches!(result.kind(), TypeKind::Bit | TypeKind::Chandle);
    let (width, signed) = casts::expr_type_from_type(&tree.functions, result, env)
        .filter(|shape| (1..=64).contains(&shape.width))
        .map(|shape| (shape.width as u32, shape.signed))
        .ok_or_else(|| unsupported("of a value other than an integral of at most 64 bits"))?;
    // Its formals are inputs (18.5.11), each given.
    let subroutine = native.subroutine(method.native);
    let ports = match &subroutine.ports {
        Some(ports) => ports.clone(),
        None => subroutine
            .port_declarations
            .iter()
            .flat_map(|declaration| declaration.ports.iter().cloned())
            .collect(),
    };
    if ports
        .iter()
        .any(|port| !matches!(port.direction, None | Some(Direction::Input)))
    {
        return Err(unsupported("with an argument other than an input"));
    }
    // The actuals of the formals, bound as in any call (13.5.4): an omitted
    // one with a default takes it (13.5.3).
    let formals = ports
        .iter()
        .map(|port| {
            port.name
                .as_ref()
                .map(|name| instances::normalize_identifier(&name.spelling))
        })
        .collect::<Vec<_>>();
    let (bound, _) = expression_binding::formal_arguments(&name, actuals, &formals)
        .map_err(|error| error.to_string())?;
    let mut arguments = Vec::with_capacity(bound.len());
    for (actual, port) in bound.into_iter().zip(&ports) {
        arguments.push(match actual {
            Some(actual) => Some(bind(session, actual)?),
            None if port.default.is_some() => None,
            None => {
                return Err(unsupported(
                    "called without an argument for a formal without a default",
                ));
            }
        });
    }
    if arguments
        .iter()
        .flatten()
        .any(|argument| emit(argument.clone(), None).reads_input_from(RESULT_INPUTS))
    {
        return Err(unsupported("whose argument calls a function"));
    }
    // The guards it is made under, all of which must hold.
    let guard = session
        .constraint_guards
        .iter()
        .cloned()
        .reduce(|left, right| ConstraintExpr::Binary {
            op: "&&".into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        });
    if guard
        .as_ref()
        .is_some_and(|guard| guard.reads_input_from(RESULT_INPUTS))
    {
        return Err(unsupported("under a guard that calls a function"));
    }
    let call = ConstraintCall {
        block,
        live: true,
        method: method.name.clone(),
        owner: method.owner.clone(),
        is_virtual: method.is_virtual,
        arguments: arguments
            .into_iter()
            .map(|argument| {
                argument.map(|argument| {
                    let (width, signed) = (argument.width, argument.signed);
                    (emit(argument, None), width, signed)
                })
            })
            .collect(),
        guard,
        width,
        signed,
        four_state,
    };
    let (_, calls, _) = session
        .constraint_methods
        .last_mut()
        .expect("the class's methods");
    calls.push(call);
    let index = calls.len() - 1;
    Ok((index, width, signed))
}

/// `expr` with each value of a call of a function (see [`RESULT_INPUTS`])
/// the input `first + k` (see `Class::constraint_calls`).
pub(in crate::ast) fn call_results(mut expr: ConstraintExpr, first: usize) -> ConstraintExpr {
    expr.rewrite(&mut |expr| {
        if let ConstraintExpr::Input { index, width } = *expr
            && (RESULT_INPUTS..SCOPE_FIELDS).contains(&index)
        {
            *expr = ConstraintExpr::Input {
                index: first + (index - RESULT_INPUTS),
                width,
            };
        }
    });
    expr
}

/// The first input index that stands for variable `i` of a scope randomize
/// call while its constraints are bound (see [`scope_fields`]).
pub(in crate::ast) const SCOPE_FIELDS: usize = 1 << 29;

/// `expr` with each input that stands for a variable of a scope randomize
/// call (see [`SCOPE_FIELDS`]) the field of the variable.
pub(in crate::ast) fn scope_fields(mut expr: ConstraintExpr) -> ConstraintExpr {
    expr.rewrite(&mut |expr| {
        if let ConstraintExpr::Input { index, .. } = *expr
            && index >= SCOPE_FIELDS
        {
            *expr = ConstraintExpr::Field(index - SCOPE_FIELDS);
        }
    });
    expr
}

/// The class handle `id` names in a constraint, `null` or a handle
/// property, as an unsigned integer; `None` for another expression.
#[allow(clippy::too_many_arguments)]
fn handle_operand(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<Typed>, String> {
    let native = tree.declarations();
    let id = crate::ast::expression_binding::ungroup(id, native);
    let width = 8 * celox_heap::HANDLE_BYTES as u32;
    let leaf = |expr| Typed {
        kind: Kind::Leaf(expr),
        width,
        signed: false,
    };
    Ok(match &native.expression(id).kind {
        ExprKind::Literal(text) if text == "null" => Some(leaf(ConstraintExpr::Literal(
            width,
            celox_heap::NULL as u64,
        ))),
        // A handle property, as other constraint operands name it.
        ExprKind::Name(_) | ExprKind::Member { .. } => {
            let Some((name, lineage)) = member_name(id, native, names, inputs, loops)? else {
                return Ok(None);
            };
            match super::member(&name, lineage, properties, names.aggregates, names.statics) {
                Some(super::Member::Instance(index))
                    if properties[index].r#type.class_in(&session.types).is_some() =>
                {
                    let property = &properties[index];
                    visible(property, tree, session, env)?;
                    Some(leaf(ConstraintExpr::Handle(property.field.offset)))
                }
                // A static handle is its value when the call starts, an
                // input of the program (see `classes::StaticInput`).
                Some(super::Member::Static(index))
                    if names.statics[index]
                        .r#type
                        .class_in(&session.types)
                        .is_some() =>
                {
                    let property = &names.statics[index];
                    accessible(
                        &property.owner,
                        &property.name,
                        property.local,
                        property.protected,
                        tree,
                        session,
                        env,
                    )?;
                    let index = names
                        .inputs
                        .iter()
                        .position(|input| input.variable == property.variable)
                        .ok_or("a static class handle without its input")?;
                    Some(leaf(ConstraintExpr::Input { index, width }))
                }
                _ => None,
            }
        }
        _ => None,
    })
}

/// The array property and method that `id`, `a.m` with or without
/// parentheses, names (IEEE 1800-2023 7.5.2, 7.12): when `a` is an array
/// property of the class, by its type. A member of the object a handle `a`
/// refers to is no array method, though named alike (`h.size`, `h.sum`).
fn array_method(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    properties: &[Property],
    names: &Names<'_>,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Option<(usize, String)> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
    else {
        return None;
    };
    if separator != "." {
        return None;
    }
    array_property(*expression, native, properties, names, inputs, loops)
        .ok()?
        .map(|(index, _)| (index, instances::normalize_identifier(&name.spelling)))
}

/// The array property of the class that constraint operand `id` names, by
/// its index, as [`member_name`] resolves it.
fn array_property<'p>(
    id: ExprId,
    native: &celox_sv_syntax::Declarations,
    properties: &'p [Property],
    names: &Names<'_>,
    inputs: &HashMap<String, CallerInput>,
    loops: &[Loop],
) -> Result<Option<(usize, &'p Property)>, String> {
    let Some((name, lineage)) = member_name(id, native, names, inputs, loops)? else {
        return Ok(None);
    };
    Ok(instance(&name, lineage, properties, names)
        .filter(|(_, property)| property.constraint_array()))
}

/// The sum of the elements of array property `index` of the class, of the
/// elements' type (IEEE 1800-2023 7.12.3): of a rand array, a sum of solver
/// variables; of another, a value.
fn sum(
    index: usize,
    properties: &[Property],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<Typed, String> {
    let property = &properties[index];
    let element = property.array_storage().expect("an array property");
    if !solver_element(element, tree, session, env) {
        return Err(
            "sum of an array of elements other than 2-state integral ones of at most 64 bits"
                .into(),
        );
    }
    visible(property, tree, session, env)?;
    let array = properties[..index]
        .iter()
        .filter(|property| property.constraint_array())
        .count();
    Ok(Typed {
        kind: Kind::Leaf(ConstraintExpr::Sum(array)),
        width: element.layout.width,
        signed: element.layout.signed,
    })
}

/// Method `method` of array property `array` of the class (see
/// [`array_method`]).
fn array_value(
    array: usize,
    method: &str,
    properties: &[Property],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<Typed, String> {
    match method {
        "size" => size(array, properties),
        "sum" => sum(array, properties, tree, session, env),
        _ => Err(format!(
            "constraint array method `{}.{method}`",
            properties[array].name
        )),
    }
}

/// The size of array property `index` of the class among the arrays of the
/// program.
fn size(index: usize, properties: &[Property]) -> Result<Typed, String> {
    if !properties[index].sequence() {
        return Err(format!(
            "constraint variable `{}.size`",
            properties[index].name
        ));
    }
    let array = properties[..index]
        .iter()
        .filter(|property| property.constraint_array())
        .count();
    Ok(Typed {
        kind: Kind::Leaf(ConstraintExpr::Size(array)),
        width: 32,
        signed: true,
    })
}

/// Whether a constraint may name property `property` as a value of the
/// solver, by its semantic type: a 2-state integral one of at most 64 bits
/// (IEEE 1800-2023 18.3). Properties of the class and members of objects a
/// loop reaches take the same check; every other kind of type is named
/// here, so that a new one has to say whether the solver holds it.
pub(in crate::ast) fn solver_holds(
    property: &Property,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    solver_type(&property.r#type, tree, session, env)
}

/// Whether the solver holds instance property `property` of the class as a
/// field of its program: as [`solver_holds`], or a 4-state integral one of
/// at most 64 bits, which the solver gives a 2-state value and takes while
/// it has no X or Z bits (IEEE 1800-2023 18.3; see `Field::four_state`).
fn solver_field(
    property: &Property,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    field_type(&property.r#type, true, tree, session, env)
}

/// Whether the solver holds a value of type `ty`, by its semantic type: a
/// 2-state integral one of at most 64 bits. Every other kind of type is
/// named here, so that a new one has to say whether the solver holds it.
pub(in crate::ast) fn solver_type(
    ty: &Type,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    field_type(ty, false, tree, session, env)
}

/// [`solver_type`], also of a 4-state integral type with `four_state`.
fn field_type(
    ty: &Type,
    four_state: bool,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    use super::super::semantic_types::{TyData, of_type};
    let id = of_type(&mut session.types, &tree.functions, ty, env)
        .ok_or("a value of a type a constraint does not take")?;
    match session.types.data(id) {
        TyData::Packed(packed) if packed.four_state && !four_state => {
            Err("4-state class property in a constraint".into())
        }
        TyData::Packed(packed) if packed.bits() <= 64 => Ok(()),
        TyData::Real | TyData::Shortreal => Err("a real in a constraint".into()),
        // A chandle has no integral value to constrain (6.14).
        TyData::Chandle => Err("a chandle in a constraint".into()),
        TyData::Packed(_)
        | TyData::String
        | TyData::Null
        | TyData::Class(_)
        | TyData::VirtualInterface { .. }
        | TyData::UnpackedStruct(_)
        | TyData::Array { .. } => {
            Err("a handle, string, container or property wider than 64 bits in a constraint".into())
        }
    }
}

/// Whether the solver holds the elements of container `container` as
/// values: those of a type [`solver_type`] takes, an enumeration with its
/// values.
fn solver_element(
    container: &ContainerType,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> bool {
    container.layout.handle.is_none()
        && container.record.is_none()
        && !container.element_enumeration(&session.types).unresolved()
        && solver_type(&container.element, tree, session, env).is_ok()
}

/// The value of member `name` of the class whose lineage, from the class
/// on, is `lineage`, if it is a property of the class (IEEE 1800-2023 18.5):
/// the variable of an instance or a static rand one, the value of another
/// static one.
fn class_member(
    name: &str,
    lineage: &[String],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
) -> Result<Option<Typed>, String> {
    match super::member(name, lineage, properties, names.aggregates, names.statics) {
        Some(super::Member::Static(index)) => {
            // A static property of the class: the variable of the rand one
            // the call solves, the value of another.
            let property = &names.statics[index];
            accessible(
                &property.owner,
                &property.name,
                property.local,
                property.protected,
                tree,
                session,
                env,
            )?;
            // A rand one is a field of the program, which may be 4-state as
            // an instance one (see `solver_field`).
            field_type(&property.r#type, property.rand, tree, session, env)?;
            Ok(Some(if property.rand {
                let slot = names
                    .slots
                    .iter()
                    .position(|slot| slot.variable == property.variable)
                    .ok_or("a static rand property without its slot")?;
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Static(slot)),
                    width: names.slots[slot].field.width,
                    signed: names.slots[slot].field.signed,
                }
            } else {
                let index = names
                    .inputs
                    .iter()
                    .position(|input| input.variable == property.variable)
                    .ok_or("a static property wider than 64 bits in a constraint")?;
                let input = &names.inputs[index];
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Input {
                        index,
                        width: input.width,
                    }),
                    width: input.width,
                    signed: input.signed,
                }
            }))
        }
        Some(super::Member::Instance(index)) => {
            let property = &properties[index];
            visible(property, tree, session, env)?;
            solver_field(property, tree, session, env)?;
            // The field of the property among those of the program.
            let field = properties[..index]
                .iter()
                .filter(|property| constrainable(property))
                .count();
            Ok(Some(Typed {
                kind: Kind::Leaf(ConstraintExpr::Field(field)),
                width: property.field.width,
                signed: property.field.signed,
            }))
        }
        Some(super::Member::Aggregate(_)) | None => Ok(None),
    }
}

/// Constraint `id` of a class with `properties` and `names`, whose other
/// names are the variables of a randomize call, input `index` of the
/// program, of `width` bits and `signed`, by `inputs`.
pub(in crate::ast) fn bind_inputs(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
) -> Result<ConstraintExpr, String> {
    #[allow(clippy::too_many_arguments)]
    fn expression(
        id: ExprId,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        properties: &[Property],
        names: &Names<'_>,
        env: &ConstEnv,
        inputs: &HashMap<String, CallerInput>,
        loops: &[Loop],
        depth: usize,
        budget: &mut usize,
    ) -> Result<Typed, String> {
        if depth > 64 {
            return Err("constraint expression nesting limit".into());
        }
        charge(budget)?;
        let recurse = |session: &mut Session, budget: &mut usize, id| {
            expression(
                id,
                tree,
                session,
                properties,
                names,
                env,
                inputs,
                loops,
                depth + 1,
                budget,
            )
        };
        let native = tree.declarations();
        // A value the call computes and supplies (18.7), such as `req.rw`.
        if let Some(input) = inputs.get(&expression_key(id, native)) {
            return Ok(Typed {
                kind: Kind::Leaf(ConstraintExpr::Input {
                    index: input.index,
                    width: input.width,
                }),
                width: input.width,
                signed: input.signed,
            });
        }
        if let Some(element) = element_at(id, tree, session, properties, names, env, inputs, loops)?
        {
            return Ok(element);
        }
        if let Some(element) = element(id, tree, session, properties, names, env, inputs, loops)? {
            return Ok(element);
        }
        // A guard compares class handles, such as with `null`: their values
        // when the call starts, which no randomization changes (IEEE
        // 1800-2023 18.5.13).
        if let ExprKind::Binary {
            left,
            operator,
            right,
        } = &native.expression(id).kind
            && matches!(operator.as_str(), "==" | "!=" | "===" | "!==")
            && let Some(a) =
                handle_operand(*left, tree, session, properties, names, env, inputs, loops)?
            && let Some(b) =
                handle_operand(*right, tree, session, properties, names, env, inputs, loops)?
        {
            let operator = if operator.starts_with('=') {
                "=="
            } else {
                "!="
            };
            return Ok(relation(operator, a, b));
        }
        Ok(match &native.expression(id).kind {
            ExprKind::Group(id) => return recurse(session, budget, *id),
            // A loop over the elements of a state array (IEEE 1800-2023
            // 18.5.8.1), solved with the elements it has.
            ExprKind::ConstraintForeach {
                array,
                variables,
                body,
            } => {
                let Some((name, _)) = member_name(*array, native, names, inputs, loops)? else {
                    return Err("constraint foreach over an array other than a property".into());
                };
                let [Some(variable)] = variables.as_slice() else {
                    return Err("constraint foreach over more than one dimension".into());
                };
                let (index, property) =
                    array_property(*array, native, properties, names, inputs, loops)?
                        .ok_or_else(|| format!("constraint foreach over `{name}`"))?;
                // The elements of a rand array are solver variables
                // (18.5.8.1), which the solver holds as its scalars.
                if (property.rand || property.randc) && !random_array(property, tree, session, env)
                {
                    return Err(
                        "constraint foreach over a rand array of elements other than 2-state integral ones of at most 64 bits"
                            .into(),
                    );
                }
                // The solver iterates positions: those of a fixed-size array
                // are its indices only from 0 (IEEE 1800-2023 18.5.8.1).
                if property
                    .storage
                    .as_ref()
                    .is_some_and(|storage| storage.fixed.is_some_and(|(left, _)| left != 0))
                    || property.storage.as_ref().is_some_and(|storage| {
                        storage.fixed.is_some_and(|(left, right)| left > right)
                    })
                {
                    return Err(
                        "constraint foreach over a fixed-size array whose range is not [0:N-1]"
                            .into(),
                    );
                }
                visible(property, tree, session, env)?;
                let array = properties[..index]
                    .iter()
                    .filter(|property| property.constraint_array())
                    .count();
                let mut inner = loops.to_vec();
                inner.push(Loop {
                    variable: instances::normalize_identifier(&variable.spelling),
                    array: index,
                    element: property.array_storage().expect("an array property").clone(),
                });
                let body = expression(
                    *body,
                    tree,
                    session,
                    properties,
                    names,
                    env,
                    inputs,
                    &inner,
                    depth + 1,
                    budget,
                )?;
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Foreach {
                        array,
                        body: Box::new(emit(body, None)),
                    }),
                    width: 1,
                    signed: false,
                }
            }
            ExprKind::Name(name)
                if loops.iter().any(|candidate| {
                    candidate.variable == instances::normalize_identifier(&name.spelling)
                }) =>
            {
                let name = instances::normalize_identifier(&name.spelling);
                let level = loops
                    .iter()
                    .rposition(|candidate| candidate.variable == name)
                    .expect("a loop variable");
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Index {
                        level: level as u32,
                    }),
                    width: 32,
                    signed: true,
                }
            }
            // A name of the caller of inline constraints, also one of a
            // member that a restricted block does not list (18.7).
            ExprKind::Name(name)
                if inputs.contains_key(&instances::normalize_identifier(&name.spelling)) =>
            {
                let input = &inputs[&instances::normalize_identifier(&name.spelling)];
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Input {
                        index: input.index,
                        width: input.width,
                    }),
                    width: input.width,
                    signed: input.signed,
                }
            }
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                if let Some((member, lineage)) = member_name(id, native, names, inputs, loops)?
                    && let Some(member) =
                        class_member(&member, lineage, tree, session, properties, names, env)?
                {
                    member
                } else if let Some(input) = inputs.get(&name) {
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Input {
                            index: input.index,
                            width: input.width,
                        }),
                        width: input.width,
                        signed: input.signed,
                    }
                } else if let Some(&value) = env.get(&name)
                    && let Some(&width) = env.get(&dimensions::parameter_width_marker(&name))
                    && let Ok(width) = u32::try_from(width)
                    && (1..=64).contains(&width)
                {
                    // A constant whose type is known, such as an enumeration
                    // constant (IEEE 1800-2023 6.19), is a literal of it.
                    let signed = env
                        .get(&dimensions::parameter_signed_marker(&name))
                        .is_some_and(|signed| *signed != 0);
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Literal(
                            width,
                            value as u64 & (u64::MAX >> (64 - width)),
                        )),
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
            // A method of an array property, with or without the
            // parentheses: its size (IEEE 1800-2023 7.5.2, 7.10.2.1, 18.4) or
            // the sum of its elements, of their type (7.12.3, 18.5.8.2).
            ExprKind::Call {
                expression,
                arguments,
            } if arguments.is_empty()
                && array_method(*expression, native, properties, names, inputs, loops)
                    .is_some() =>
            {
                let (array, method) =
                    array_method(*expression, native, properties, names, inputs, loops).unwrap();
                return array_value(array, &method, properties, tree, session, env);
            }
            ExprKind::Member { .. }
                if array_method(id, native, properties, names, inputs, loops).is_some() =>
            {
                let (array, method) =
                    array_method(id, native, properties, names, inputs, loops).unwrap();
                return array_value(array, &method, properties, tree, session, env);
            }
            // A value of the caller the call supplies by a path: `local::`
            // (18.7), or a member of the caller's object in a scope
            // randomize call (18.12).
            ExprKind::Member { .. }
                if caller_key(id, native, true).is_some_and(|key| inputs.contains_key(&key)) =>
            {
                let input = inputs[&caller_key(id, native, true).unwrap()];
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Input {
                        index: input.index,
                        width: input.width,
                    }),
                    width: input.width,
                    signed: input.signed,
                }
            }
            // A property qualified by `this` or `super` (IEEE 1800-2023
            // 8.11, 8.15), or by a class scope of the class or a base (8.23):
            // one of the class, or of its base, also in inline constraints,
            // where it never names the caller's (18.7); or by the variable
            // that holds the object the call randomizes (see
            // `Names::receiver`).
            ExprKind::Member { .. } if member_name(id, native, names, inputs, loops)?.is_some() => {
                let (member, lineage) =
                    member_name(id, native, names, inputs, loops)?.expect("a member name");
                class_member(&member, lineage, tree, session, properties, names, env)?
                    .ok_or_else(|| format!("constraint variable `{member}`"))?
            }
            ExprKind::Member {
                expression,
                separator,
                ..
            } if separator == "::"
                && matches!(&native.expression(*expression).kind, ExprKind::Name(root) if root.spelling == "local") =>
            {
                return Err("inline local variable".into());
            }
            // A member of the object a handle property of the class refers
            // to (IEEE 1800-2023 18.5.9): a variable of that object, solved
            // with the class's when the handle is rand (18.4).
            ExprKind::Member { .. }
                if handle_member(id, tree, session, properties, names, env, inputs, loops)?
                    .is_some() =>
            {
                let (root, property) =
                    handle_member(id, tree, session, properties, names, env, inputs, loops)?
                        .expect("a handle member");
                // The handle is a member the constraint accesses, as the
                // member of its object is (8.18).
                let (handle, input) = match root {
                    PathHandle::Instance(handle) => {
                        visible(handle, tree, session, env)?;
                        (handle.field.offset, None)
                    }
                    PathHandle::Static(input) => (0, Some(input)),
                };
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Member {
                        handle,
                        member: property.field.offset,
                        width: property.field.width,
                        input,
                    }),
                    width: property.field.width,
                    signed: property.field.signed,
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
                let value = recurse(session, budget, *expression)?;
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
                let value = recurse(session, budget, *left)?;
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
                        DimensionKind::Index(id) => (recurse(session, budget, id)?, None),
                        DimensionKind::Range { left, right } => (
                            recurse(session, budget, left)?,
                            Some(recurse(session, budget, right)?),
                        ),
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
                balanced("||", alternatives).ok_or("empty constraint inside set")?
            }
            // A uniqueness constraint (IEEE 1800-2023 18.5.4): its members,
            // singular variables of equivalent types, take pairwise
            // different values; fewer than two have no effect.
            ExprKind::Unique(items) => {
                let mut members = Vec::new();
                let mut types = Vec::new();
                for item in items {
                    let celox_sv_syntax::DimensionKind::Index(id) = item.kind else {
                        return Err(
                            "a value range in a uniqueness constraint, whose members are variables (IEEE 1800-2023 18.5.4)"
                                .into(),
                        );
                    };
                    let ExprKind::Name(name) = &native
                        .expression(expression_binding::ungroup(id, native))
                        .kind
                    else {
                        return Err(
                            "a uniqueness constraint member that is not a variable (IEEE 1800-2023 18.5.4)"
                                .into(),
                        );
                    };
                    let name = instances::normalize_identifier(&name.spelling);
                    // A property of the class, or a variable the call
                    // supplies, such as one a scope randomize call solves
                    // (18.12): the name binds as anywhere else.
                    if let Some(property) = properties
                        .iter()
                        .rev()
                        .find(|property| property.name == name)
                    {
                        if property.randc {
                            return Err(
                                "a randc variable in a uniqueness constraint (IEEE 1800-2023 18.5.4)"
                                    .into(),
                            );
                        }
                        if property.r#type.container().is_some()
                            || !property.r#type.unpacked_ranges.is_empty()
                        {
                            return Err("an array in a uniqueness constraint".into());
                        }
                        types.push(semantic_types::of_type(
                            &mut session.types,
                            &tree.functions,
                            &property.r#type,
                            env,
                        ));
                    } else if let Some(input) = inputs.get(&name) {
                        types.push(input.ty);
                    } else {
                        return Err(
                            "a uniqueness constraint member other than a variable of the class or the call"
                                .into(),
                        );
                    }
                    members.push(recurse(session, budget, id)?);
                }
                // Members of equivalent types (6.22.2), as the type table
                // tells them: two enumerations only if they are one.
                if types.windows(2).any(|pair| match (pair[0], pair[1]) {
                    (Some(left), Some(right)) => !session.types.equivalent(left, right),
                    _ => true,
                }) {
                    return Err(
                        "a uniqueness constraint over members of types that are not equivalent (IEEE 1800-2023 18.5.4)"
                            .into(),
                    );
                }
                let mut different = Vec::new();
                for (index, left) in members.iter().enumerate() {
                    for right in &members[index + 1..] {
                        charge(budget)?;
                        different.push(relation(
                            "!=",
                            bounded_clone(left, budget)?,
                            bounded_clone(right, budget)?,
                        ));
                    }
                }
                balanced("&&", different).unwrap_or(Typed {
                    kind: Kind::Leaf(ConstraintExpr::Literal(1, 1)),
                    width: 1,
                    signed: false,
                })
            }
            // A distribution (IEEE 1800-2023 18.5.3): the value is one of an
            // item of nonzero weight, and the solver tries the items by their
            // weights, `:=` counting each value of a range, `:/` the range.
            ExprKind::Dist { expression, items } => {
                let value = recurse(session, budget, *expression)?;
                // A distribution shall not apply to a randc variable
                // (18.5.3): to no operand that reads one, however it is
                // named (`b`, `this.b`) or selected (`b[3:0]`, `b + 1`).
                // That of a member of another object (18.5.9) too.
                let mut members = Vec::new();
                typed_reads(&value, &mut |leaf| {
                    if let ConstraintExpr::Member {
                        handle,
                        member,
                        input,
                        ..
                    } = leaf
                    {
                        members.push((*handle, *member, *input));
                    }
                    false
                });
                let mut randc = false;
                for (handle, member, input) in members {
                    randc |=
                        randc_member(handle, member, input, tree, session, properties, names, env)?;
                }
                randc |= typed_reads(&value, &mut |leaf| {
                    matches!(leaf, ConstraintExpr::Field(field) if properties
                        .iter()
                        .filter(|property| constrainable(property))
                        .nth(*field)
                        .is_some_and(|property| property.randc))
                });
                if randc {
                    return Err("a distribution of a randc variable (IEEE 1800-2023 18.5.3)".into());
                }
                let mut members = Vec::new();
                let mut width = value.width;
                let mut signed = value.signed;
                // `$` as a bound of a range is the lowest or highest value
                // of the type of the expression (11.4.13).
                let extreme = |high: bool| -> Result<(Typed, Bound), String> {
                    let width = value.width;
                    if !(1..=64).contains(&width) {
                        return Err("`$` bound of a distribution over more than 64 bits".into());
                    }
                    let bound = match (value.signed, high) {
                        (true, false) => -(1i128 << (width - 1)),
                        (true, true) => (1i128 << (width - 1)) - 1,
                        (false, false) => 0,
                        (false, true) => (1i128 << width) - 1,
                    };
                    Ok((
                        Typed {
                            kind: Kind::Leaf(ConstraintExpr::Literal(
                                width,
                                bound as u64 & (u64::MAX >> (64 - width)),
                            )),
                            width,
                            signed: value.signed,
                        },
                        Bound::Value(bound),
                    ))
                };
                let unbounded = |id: ExprId| matches!(&native.expression(id).kind, ExprKind::Name(name) if name.spelling == "$");
                for item in items {
                    use celox_sv_syntax::DimensionKind;
                    let (bounds, low, high) = match item.value.kind {
                        DimensionKind::Index(id) => {
                            ((Bound::Expr(id), None), recurse(session, budget, id)?, None)
                        }
                        DimensionKind::Range { left, right } => {
                            let (low, low_bound) = if unbounded(left) {
                                extreme(false)?
                            } else {
                                (recurse(session, budget, left)?, Bound::Expr(left))
                            };
                            let (high, high_bound) = if unbounded(right) {
                                extreme(true)?
                            } else {
                                (recurse(session, budget, right)?, Bound::Expr(right))
                            };
                            ((low_bound, Some(high_bound)), low, Some(high))
                        }
                        _ => return Err("distribution range".into()),
                    };
                    for operand in std::iter::once(&low).chain(high.iter()) {
                        width = width.max(operand.width);
                        signed &= operand.signed;
                    }
                    members.push((low, high, bounds, item.weight));
                }
                // The weights, and the bounds of ranges weighted per value,
                // are constant expressions; a weight is unsigned (18.5.3),
                // a bound of the type the items compare in.
                let mut weighted = Vec::new();
                for (low, high, (low_id, high_id), weight) in members {
                    let weight = match weight {
                        None => (num_bigint::BigUint::from(1u8), false),
                        Some(weight) => {
                            // A weight is read as unsigned (18.5.3), every
                            // bit of its type kept.
                            let value = constant_unsigned(weight.value, tree, session, env)
                                .or_else(|| {
                                    // The numeric value of a form the typed
                                    // evaluation does not take, which is
                                    // its bits when it is not negative.
                                    constant(weight.value, tree, session, env)
                                        .and_then(|value| u128::try_from(value).ok())
                                        .map(num_bigint::BigUint::from)
                                })
                                .ok_or("distribution weight other than a constant")?;
                            (value, weight.per_range)
                        }
                    };
                    let weight = match (high_id, weight) {
                        (Some(high_id), (weight, false)) => {
                            let bound = |session: &mut Session, bound| {
                                match bound {
                                Bound::Value(value) => Ok(in_type(value, width, signed)),
                                Bound::Expr(id) => constant(id, tree, session, env)
                                    .map(|value| in_type(value, width, signed))
                                    .ok_or(
                                        "distribution range with nonconstant bounds weighted per value",
                                    ),
                            }
                            };
                            let low = bound(session, low_id)?;
                            let high = bound(session, high_id)?;
                            // A range whose low bound is above its high one
                            // is empty (11.4.13).
                            let count = if high >= low {
                                num_bigint::BigUint::from(high.abs_diff(low)) + 1u8
                            } else {
                                num_bigint::BigUint::ZERO
                            };
                            weight * count
                        }
                        (_, (weight, _)) => weight,
                    };
                    weighted.push((low, high, weight));
                }
                let members = weighted;
                let value = sized(value, width, signed);
                let mut alternatives = Vec::new();
                let mut choices = Vec::new();
                for (low, high, weight) in members {
                    // A value of total weight zero is excluded (18.5.3).
                    if weight == num_bigint::BigUint::ZERO {
                        continue;
                    }
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
                    choices.push(celox_heap::randomization::DistChoice {
                        predicate: emit(bounded_clone(&predicate, budget)?, None),
                        weight,
                    });
                    alternatives.push(predicate);
                }
                // Every item of weight zero: no value is allowed.
                let membership = balanced("||", alternatives).unwrap_or(Typed {
                    kind: Kind::Leaf(ConstraintExpr::Literal(1, 0)),
                    width: 1,
                    signed: false,
                });
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Dist {
                        membership: Box::new(emit(membership, None)),
                        choices,
                    }),
                    width: 1,
                    signed: false,
                }
            }
            // A shift has the type of its left operand; the right operand
            // is self-determined, of its own type, and its value is read as
            // unsigned (IEEE 1800-2023 11.4.10, 11.6.1).
            ExprKind::Binary {
                left,
                operator,
                right,
            } if matches!(operator.as_str(), "<<" | ">>" | "<<<" | ">>>") => {
                let a = recurse(session, budget, *left)?;
                let b = recurse(session, budget, *right)?;
                Typed {
                    width: a.width,
                    signed: a.signed,
                    kind: Kind::Binary(operator.clone(), Box::new(a), Box::new(b)),
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
                let a = recurse(session, budget, *left)?;
                let logical = matches!(operator.as_str(), "&&" | "||");
                // The right operand of `&&` and `||` is a guarded one: a
                // call in it runs only while the left operand lets it.
                if logical {
                    let guard = emit(a.clone(), None);
                    session.constraint_guards.push(if operator == "||" {
                        ConstraintExpr::Unary {
                            op: "!".into(),
                            value: Box::new(guard),
                        }
                    } else {
                        guard
                    });
                }
                let b = recurse(session, budget, *right);
                if logical {
                    session.constraint_guards.pop();
                }
                let b = b?;
                let width = a.width.max(b.width);
                let signed = a.signed && b.signed;
                let comparison =
                    logical || matches!(operator.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=");
                Typed {
                    width: if comparison { 1 } else { width },
                    signed: !comparison && signed,
                    kind: Kind::Binary(operator.clone(), Box::new(a), Box::new(b)),
                }
            }
            ExprKind::Call {
                expression: callee,
                arguments,
            } => {
                if !loops.is_empty() {
                    return Err("a function call in a constraint loop".into());
                }
                let (index, width, signed) = function_call(
                    *callee,
                    arguments,
                    &mut |session: &mut Session, value: ExprId| recurse(session, budget, value),
                    tree,
                    session,
                    names,
                    env,
                    inputs,
                    loops,
                )?;
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Input {
                        index: RESULT_INPUTS + index,
                        width,
                    }),
                    width,
                    signed,
                }
            }
            // An element of an array of the caller at a position the
            // caller gives, a value the call supplies (18.7): not one at the
            // position of a constraint loop's variable.
            ExprKind::Select { .. }
                if caller_key(id, native, true).is_some_and(|key| inputs.contains_key(&key))
                    && element_names(id, native).is_some_and(|(_, index)| {
                        !index.is_some_and(|index| {
                            loops.iter().any(|looped| looped.variable == index)
                        })
                    }) =>
            {
                let input = inputs[&caller_key(id, native, true).unwrap()];
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Input {
                        index: input.index,
                        width: input.width,
                    }),
                    width: input.width,
                    signed: input.signed,
                }
            }
            // A select of a property at constant positions: its bits, an
            // unsigned value unless they are an element of a signed type
            // (IEEE 1800-2023 7.4.1, 11.5.1, 11.8.1).
            ExprKind::Select {
                expression: operand,
                dimension,
            } if !part_selected(*operand, native)
                && select_operand_type(
                    *operand, tree, session, properties, names, env, inputs, loops,
                )
                .is_some() =>
            {
                let ty = select_operand_type(
                    *operand, tree, session, properties, names, env, inputs, loops,
                )
                .expect("a select operand type");
                let value = recurse(session, budget, *operand)?;
                let (lowest, width, signed) =
                    selected_bits(dimension, ty, value.width, tree, session, env)?;
                let value = emit(value, None);
                let shifted = if lowest == 0 {
                    value
                } else {
                    ConstraintExpr::Binary {
                        op: ">>".into(),
                        left: Box::new(value),
                        right: Box::new(ConstraintExpr::Literal(32, u64::from(lowest))),
                        signed: false,
                    }
                };
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Resize {
                        value: Box::new(shifted),
                        width,
                        signed: false,
                    }),
                    width,
                    signed,
                }
            }
            // Every node kind is listed, so that a new one has to say how a
            // constraint takes it.
            ExprKind::Member { .. }
            | ExprKind::Unary { .. }
            | ExprKind::Binary { .. }
            | ExprKind::MinTypMax { .. }
            | ExprKind::Type(_)
            | ExprKind::Conditional { .. }
            | ExprKind::Assignment { .. }
            | ExprKind::Tagged { .. }
            | ExprKind::Copy(_)
            | ExprKind::ArrayMethodWith { .. }
            | ExprKind::RandomizeWith { .. }
            | ExprKind::Parameterized { .. }
            | ExprKind::Select { .. }
            | ExprKind::Cast { .. }
            | ExprKind::Concatenation(_)
            | ExprKind::Replication { .. }
            | ExprKind::Pattern { .. }
            | ExprKind::PatternMatch { .. }
            | ExprKind::Set(_)
            | ExprKind::Streaming { .. }
            | ExprKind::StreamSelect { .. } => {
                return Err("unsupported scalar constraint expression".into());
            }
            ExprKind::SoftConstraint { .. } | ExprKind::DisableSoft { .. } => {
                return Err("soft constraint in a constraint loop".into());
            }
            ExprKind::SolveBefore { .. } => {
                return Err("solve before inside a constraint expression".into());
            }
        })
    }
    // The target of `disable soft h.m`, a member of the object a rand
    // handle refers to: whether it is a random variable (18.5.13.2).
    let random_member = match &tree.declarations().expression(id).kind {
        ExprKind::DisableSoft { variable, .. } => {
            random_member(*variable, tree, session, properties, names, env, inputs)?
        }
        _ => false,
    };
    // Each variable of `solve before`, as `h.m` too, likewise (18.5.9).
    let ordered_members = match &tree.declarations().expression(id).kind {
        ExprKind::SolveBefore { before, after } => before
            .iter()
            .chain(after)
            .map(|&variable| {
                self::random_member(variable, tree, session, properties, names, env, inputs)
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => Vec::new(),
    };
    let mut bound = |id| {
        expression(
            id,
            tree,
            session,
            properties,
            names,
            env,
            inputs,
            &[],
            0,
            &mut 16_384,
        )
        .map(|value| emit(value, None))
    };
    // A soft constraint and `disable soft` take the priority of their place
    // among the constraints of the class (see [`prioritize`]).
    match tree.declarations().expression(id).kind {
        ExprKind::SoftConstraint { expression } => {
            let value = bound(expression)?;
            // Soft constraints may not be specified for randc variables
            // (18.5.13.1).
            if names_randc(&value, properties) {
                return Err("a soft constraint on a randc variable".into());
            }
            Ok(ConstraintExpr::Soft {
                priority: 0,
                value: Box::new(value),
            })
        }
        ExprKind::DisableSoft {
            variable,
            condition,
        } => match selected_variable(bound(variable)?) {
            variable @ (ConstraintExpr::Field(_)
            | ConstraintExpr::Static(_)
            | ConstraintExpr::Size(_)) => Ok(ConstraintExpr::DisableSoft {
                priority: 0,
                variable: Box::new(variable),
                condition: condition.map(&mut bound).transpose()?.map(Box::new),
            }),
            variable @ ConstraintExpr::Member { .. } if random_member => {
                Ok(ConstraintExpr::DisableSoft {
                    priority: 0,
                    variable: Box::new(variable),
                    condition: condition.map(&mut bound).transpose()?.map(Box::new),
                })
            }
            _ => Err("disable soft of other than a random property or array size".into()),
        },
        // An order of random variables (18.5.9) leaves the solutions as
        // they are, and the search keeps no uniform distribution that it
        // would shape: each must be a rand variable or the size of a rand
        // array, and the constraint holds.
        ExprKind::SolveBefore {
            ref before,
            ref after,
        } => {
            for (position, &variable) in before.iter().chain(after).enumerate() {
                // The variable it names, a select of one too (Syntax 18-8),
                // as the constraint's other expressions name it.
                let mut variables = Vec::new();
                operand_variables(
                    &bound(variable)?,
                    properties,
                    ordered_members[position],
                    &mut variables,
                );
                if variables != [true] {
                    return Err(
                        "solve before of other than a rand variable, a select of one or the size of a rand array"
                            .into(),
                    );
                }
            }
            Ok(ConstraintExpr::Literal(1, 1))
        }
        _ => bound(id),
    }
}

/// Whether `variable`, an operand as the constraint's other expressions
/// bind it, is a random variable of the solve that is not randc, or the
/// size of a rand array (IEEE 1800-2023 18.5.9); `member` tells it for a
/// member of the object a rand handle refers to (see [`random_member`]).
fn random_variable(variable: &ConstraintExpr, properties: &[Property], member: bool) -> bool {
    match variable {
        ConstraintExpr::Field(field) => properties
            .iter()
            .filter(|property| constrainable(property))
            .nth(*field)
            .is_some_and(|property| property.rand && !property.randc),
        // Only a rand static property is a variable of the solve.
        ConstraintExpr::Static(_) => true,
        ConstraintExpr::Size(array) => properties
            .iter()
            .filter(|property| property.constraint_array())
            .nth(*array)
            .is_some_and(|property| property.rand),
        ConstraintExpr::Member { .. } => member,
        // An argument of a scope randomize call, which [`scope_fields`]
        // makes a field of the solve: a random variable (18.12.1).
        ConstraintExpr::Input { index, .. } => *index >= SCOPE_FIELDS,
        _ => false,
    }
}

/// The variables that bound constraint operand `expr` reads, each with
/// whether it is a random variable as [`random_variable`] tells, an element
/// of a rand array too.
fn operand_variables(
    expr: &ConstraintExpr,
    properties: &[Property],
    member: bool,
    variables: &mut Vec<bool>,
) {
    match expr {
        ConstraintExpr::Field(_)
        | ConstraintExpr::Static(_)
        | ConstraintExpr::Size(_)
        | ConstraintExpr::Member { .. }
        | ConstraintExpr::Input { .. } => {
            variables.push(random_variable(expr, properties, member));
        }
        ConstraintExpr::ElementAt { array, .. } => variables.push(
            properties
                .iter()
                .filter(|property| property.constraint_array())
                .nth(*array)
                .is_some_and(|property| property.rand && !property.randc),
        ),
        ConstraintExpr::Sum(_) => variables.push(false),
        ConstraintExpr::Resize { value, .. } | ConstraintExpr::Unary { value, .. } => {
            operand_variables(value, properties, member, variables);
        }
        ConstraintExpr::Binary { left, right, .. } => {
            operand_variables(left, properties, member, variables);
            operand_variables(right, properties, member, variables);
        }
        _ => {}
    }
}

/// Whether constraint `expr` names a randc property of `properties`.
fn names_randc(expr: &ConstraintExpr, properties: &[Property]) -> bool {
    expr.any(&mut |expr| {
        matches!(expr, ConstraintExpr::Field(field) if properties
            .iter()
            .filter(|property| constrainable(property))
            .nth(*field)
            .is_some_and(|property| property.randc))
    })
}

/// Whether `id` names a rand member, not randc, of the object a rand handle
/// property of the class refers to, as `h.m` or `this.h.m`, or bits of one,
/// as `h.m[0]` (IEEE 1800-2023 18.4, 18.5.9; see [`handle_member`]).
#[allow(clippy::too_many_arguments)]
fn random_member(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, CallerInput>,
) -> Result<bool, String> {
    let native = tree.declarations();
    // A select names bits of the variable it selects.
    let mut id = expression_binding::ungroup(id, native);
    while let ExprKind::Select { expression, .. } = &native.expression(id).kind {
        id = expression_binding::ungroup(*expression, native);
    }
    Ok(
        match handle_member(id, tree, session, properties, names, env, inputs, &[]) {
            // A static handle is a state value.
            Ok(Some((PathHandle::Instance(handle), member))) => {
                handle.rand_object(&session.types) && member.rand && !member.randc
            }
            _ => false,
        },
    )
}

/// The variable whose bits bound constraint operand `expr` selects (see
/// the select binding), or `expr`: `disable soft` of a select names that
/// variable (18.5.13.2).
fn selected_variable(expr: ConstraintExpr) -> ConstraintExpr {
    match expr {
        ConstraintExpr::Resize { value, .. } => selected_variable(*value),
        ConstraintExpr::Binary {
            op, left, right, ..
        } if op == ">>" && matches!(*right, ConstraintExpr::Literal(..)) => {
            selected_variable(*left)
        }
        expr => expr,
    }
}

/// The soft constraint or `disable soft` `constraint` with priority
/// `priority` (IEEE 1800-2023 18.5.13.1); another constraint as it is.
pub(in crate::ast) fn prioritize(constraint: ConstraintExpr, priority: u64) -> ConstraintExpr {
    match constraint {
        ConstraintExpr::Soft { value, .. } => ConstraintExpr::Soft { priority, value },
        ConstraintExpr::DisableSoft {
            variable,
            condition,
            ..
        } => ConstraintExpr::DisableSoft {
            priority,
            variable,
            condition,
        },
        constraint => constraint,
    }
}

/// The priority of the constraint at `expression` of block `block` of a
/// class `depth` classes below its root class: a later constraint of a
/// block, one of a later block, by the order of the prototypes, and one of
/// a derived class are above the others (IEEE 1800-2023 18.5.13.1).
pub(in crate::ast) fn class_priority(depth: usize, block: usize, expression: usize) -> u64 {
    ((depth as u64) << 40) | ((block as u64 & 0xf_ffff) << 20) | (expression as u64 & 0xf_ffff)
}

/// The priority of constraint `expression` of an inline constraint block,
/// above every constraint of the class (18.5.13.1).
pub(in crate::ast) fn inline_priority(expression: usize) -> u64 {
    (1 << 62) | expression as u64
}

impl Class {
    /// The constraint program of the class, built once when the class is
    /// laid out (see [`Class::program`]).
    pub fn constraint_program(&self) -> Converted<Program> {
        self.program.clone()
    }

    /// Whether the class has a constraint program, without copying it.
    pub fn check_constraint_program(&self) -> Converted<()> {
        self.program.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    pub(super) fn build_constraint_program(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        env: &ConstEnv,
    ) -> Converted<Program> {
        let mut fields = Vec::new();
        let mut handles = Vec::new();
        let mut arrays = Vec::new();
        for property in &self.properties {
            // An object handle shall not be randc (18.4).
            if property.randc && property.r#type.class_in(&session.types).is_some() {
                return Err(unsupported("a randc object handle"));
            }
            if property.rand_object(&session.types) {
                handles.push(property.field.offset);
                continue;
            }
            if property.constraint_array() {
                arrays.push(array(property, tree, session, env)?);
                continue;
            }
            if !constrainable(property) {
                // A property no constraint names is state the solver does
                // not need; a random one would have to be solved.
                if property.rand || property.randc {
                    return Err(unsupported(
                        "constraints currently require scalar integral rand properties",
                    ));
                }
                continue;
            }
            // A randc property draws from the cycle of its whole domain
            // (IEEE 1800-2023 18.4.2), kept for at most 65536 values.
            if property.randc && property.cyclic_domain_size(&session.types).is_none() {
                return Err(unsupported(
                    "constraints on a randc property of more than 65536 values",
                ));
            }
            // A random enumeration takes only the values of its type (18.3).
            let mask = u64::MAX >> (64 - property.field.width);
            let domain = property
                .r#type
                .enumeration_in(&session.types)
                .domain
                .as_ref()
                // `randomize(e)` makes any property random; the solver
                // applies the domain only while the property is.
                .map(|domain| {
                    domain
                        .values()
                        .into_iter()
                        .map(|value| value.bits() as u64 & mask)
                        .collect()
                })
                .unwrap_or_default();
            fields.push(Field {
                offset: property.field.offset,
                width: property.field.width,
                random: property.rand || property.randc,
                domain,
                cyclic: property.randc,
                four_state: property.field.four_state,
            });
        }
        // The slots of the static rand properties follow (see
        // `ConstraintExpr::Static`).
        let first = fields.len();
        for slot in &self.static_slots {
            // As an instance rand property: one no constraint names is a
            // field the solver randomizes, which a handle or a value wider
            // than 64 bits cannot be. A constraint that names it checks its
            // type (see `ConstraintExpr::Static`).
            if slot.field.handle.is_some() || slot.field.width > 64 {
                return Err(unsupported(
                    "constraints currently require scalar integral rand properties",
                ));
            }
            let mask = u64::MAX >> (64 - slot.field.width);
            fields.push(Field {
                offset: slot.field.offset,
                width: slot.field.width,
                random: true,
                domain: slot
                    .r#type
                    .enumeration_in(&session.types)
                    .domain
                    .as_ref()
                    .map(|domain| {
                        domain
                            .values()
                            .into_iter()
                            .map(|value| value.bits() as u64 & mask)
                            .collect()
                    })
                    .unwrap_or_default(),
                cyclic: false,
                four_state: slot.field.four_state,
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
                    .map(|mut value| {
                        resolve_statics(&mut value, first);
                        ConstraintExpr::Enabled {
                            index: index as u32,
                            value: Box::new(value),
                        }
                    }),
            );
        }
        Ok(Program {
            fields,
            constraints,
            handles,
            arrays,
            // The arguments of the calls its live blocks make, solved first
            // while their blocks are on (18.5.11).
            // A guarded call's arguments are followed by its flag, the
            // value of its guard, which the call reads: nonzero while the
            // guard holds (see `Program::argument_guards`).
            arguments: self
                .constraint_calls
                .iter()
                .filter(|call| call.live)
                .flat_map(|call| {
                    call.arguments
                        .iter()
                        .flatten()
                        .map(|(argument, ..)| argument.clone())
                        .chain(call.guard.clone())
                        .map(|mut argument| {
                            resolve_statics(&mut argument, first);
                            ConstraintExpr::Enabled {
                                index: call.block,
                                value: Box::new(argument),
                            }
                        })
                        .collect::<Vec<_>>()
                })
                .collect(),
            argument_guards: self
                .constraint_calls
                .iter()
                .filter(|call| call.live)
                .flat_map(|call| {
                    let count =
                        call.arguments.iter().flatten().count() + usize::from(call.guard.is_some());
                    let guard = call.guard.clone().map(|mut guard| {
                        resolve_statics(&mut guard, first);
                        guard
                    });
                    std::iter::repeat_n(guard, count)
                })
                .collect(),
            // A call with 4-state values names them (see `Program::unknown_inputs`).
            unknown_inputs: Vec::new(),
        })
    }
}

/// Whether the solver can randomize array property `property`, a dynamic
/// array or queue or a fixed-size array: one of 2-state integral elements
/// of at most 64 bits.
pub(in crate::ast) fn random_array(
    property: &Property,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> bool {
    property
        .array_storage()
        .is_some_and(|container| solver_element(container, tree, session, env))
}

/// Dynamic array or queue property `property` of a constraint program: a
/// random one is resized to the size its constraints give, if any, and its
/// elements randomized (IEEE 1800-2023 18.4).
pub(in crate::ast) fn array(
    property: &Property,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<Array> {
    let container = property.array_storage().expect("an array property");
    let layout = &container.layout.clone();
    let random = property.rand || property.randc;
    if random {
        if property.randc {
            return Err(unsupported("a randc dynamic array or queue"));
        }
        if !random_array(property, tree, session, env) {
            return Err(unsupported(
                "rand dynamic arrays and queues currently require 2-state integral elements of at most 64 bits",
            ));
        }
    }
    let (kind, capacity) = match layout.kind {
        ContainerKind::Queue { capacity } => (1, capacity),
        _ => (0, 0),
    };
    let handle = match layout.handle {
        None => 0,
        Some(HandleProperty::String) => 1,
        Some(HandleProperty::Object) => 2,
    };
    let mask = u64::MAX >> (64 - layout.width.clamp(1, 64));
    Ok(Array {
        offset: property.field.offset,
        layout: celox_heap::containers::parameter(
            layout.width,
            layout.two_state,
            handle,
            kind,
            capacity,
            false,
            false,
            false,
        ),
        width: layout.width,
        random,
        domain: container
            .element_enumeration(&session.types)
            .domain
            .as_ref()
            .map(|domain| {
                domain
                    .values()
                    .into_iter()
                    .map(|value| value.bits() as u64 & mask)
                    .collect()
            })
            .unwrap_or_default(),
    })
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
