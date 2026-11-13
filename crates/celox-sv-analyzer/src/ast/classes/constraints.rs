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
    array: String,
    element: std::sync::Arc<ContainerType>,
}

/// The element `a[i]`, or the member `a[i].m` of the object it refers to,
/// that `id` names, with `i` the variable of the loop over `a`.
fn element(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    env: &ConstEnv,
    loops: &[Loop],
) -> Result<Option<Typed>, String> {
    if loops.is_empty() {
        return Ok(None);
    }
    let native = tree.declarations();
    let (select, member) = match &native.expression(id).kind {
        ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "." => (
            *expression,
            Some(instances::normalize_identifier(&name.spelling)),
        ),
        _ => (id, None),
    };
    let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(select).kind
    else {
        return Ok(None);
    };
    let ExprKind::Name(array) = &native.expression(*expression).kind else {
        return Ok(None);
    };
    let array = instances::normalize_identifier(&array.spelling);
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
    let looped = &loops[level];
    let element = &looped.element;
    let (member, width, signed) = match member {
        None => {
            if !solver_element(element, tree, session, env) {
                return Err("a constraint array element other than 2-state integral".into());
            }
            (None, element.layout.width, element.layout.signed)
        }
        Some(member) => {
            let class = element
                .element
                .class()
                .ok_or("a member of a constraint array element other than an object")?
                .to_owned();
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
                super::class(tree, session, &class, env, &TypeAliases::default())
                    .map_err(|_| format!("class `{class}`"))?
                    .property(&member)
                    .cloned()
            };
            let property = found.ok_or_else(|| format!("constraint variable `{member}`"))?;
            visible(&property, tree, session, env)?;
            solver_holds(&property, tree, session, env)?;
            (
                Some(property.field.offset),
                property.field.width,
                property.field.signed,
            )
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
    pub statics: &'a [super::StaticProperty],
    pub inputs: &'a [super::StaticInput],
    pub slots: &'a [super::StaticSlot],
}

/// The instance property that `name` denotes, by its index, if a member of
/// the class.
fn instance<'p>(
    name: &str,
    properties: &'p [Property],
    names: &Names<'_>,
) -> Option<(usize, &'p Property)> {
    match super::member(name, names.lineage, properties, names.statics)? {
        super::Member::Instance(index) => Some((index, &properties[index])),
        super::Member::Static(_) => None,
    }
}

/// Name static rand property `n` of constraint `expr` as field `first + n`
/// of its program (see `ConstraintExpr::Static`).
pub(in crate::ast) fn resolve_statics(expr: &mut ConstraintExpr, first: usize) {
    match expr {
        ConstraintExpr::Static(slot) => *expr = ConstraintExpr::Field(first + *slot),
        ConstraintExpr::Enabled { value, .. }
        | ConstraintExpr::Soft { value, .. }
        | ConstraintExpr::DisableSoft {
            variable: value, ..
        }
        | ConstraintExpr::Resize { value, .. }
        | ConstraintExpr::Unary { value, .. } => resolve_statics(value, first),
        ConstraintExpr::Foreach { body, .. } => resolve_statics(body, first),
        ConstraintExpr::Binary { left, right, .. } => {
            resolve_statics(left, first);
            resolve_statics(right, first);
        }
        ConstraintExpr::Call { args, body } => {
            for arg in args {
                resolve_statics(arg, first);
            }
            resolve_statics(body, first);
        }
        ConstraintExpr::Literal(..)
        | ConstraintExpr::Field(_)
        | ConstraintExpr::Handle(_)
        | ConstraintExpr::Argument(_)
        | ConstraintExpr::Input { .. }
        | ConstraintExpr::Size(_)
        | ConstraintExpr::Index { .. }
        | ConstraintExpr::Solved(_)
        | ConstraintExpr::Element { .. } => {}
    }
}

/// A function a constraint calls, inlined (IEEE 1800-2023 18.5.11): the
/// arguments of the call, the width and signedness of each formal, the
/// function's body with [`ConstraintExpr::Argument`] `i` for formal `i`,
/// and the width and signedness of its value.
struct FunctionCall {
    arguments: Vec<ExprId>,
    formals: Vec<(u32, bool)>,
    body: ConstraintExpr,
    width: u32,
    signed: bool,
}

/// The first input index that stands for a formal of a function a
/// constraint calls while its body is bound (see [`function_call`]).
const FORMAL_INPUTS: usize = 1 << 30;

/// The call `callee(arguments)` of a method of the class whose constraints
/// are being bound: a function of input arguments only whose body is one
/// `return` or assignment of its value, inlined. A function that preserves
/// no state and has no side effects is what a constraint may call (18.5.11);
/// a longer one stays unsupported.
#[allow(clippy::too_many_arguments)]
fn function_call(
    callee: ExprId,
    arguments: &[celox_sv_syntax::Argument],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
    inputs: &HashMap<String, (usize, u32, bool)>,
) -> Result<FunctionCall, String> {
    use celox_sv_syntax::{Direction, StatementKind};
    let native = tree.declarations();
    let ExprKind::Name(name) = &native
        .expression(crate::ast::expression_binding::ungroup(callee, native))
        .kind
    else {
        return Err("a call in a constraint other than of a function of the class".into());
    };
    let name = instances::normalize_identifier(&name.spelling);
    let method = session
        .constraint_methods
        .last()
        .and_then(|methods| methods.iter().find(|method| method.name == name))
        .cloned()
        .ok_or_else(|| format!("a call of `{name}` in a constraint"))?;
    let unsupported = |what: &str| format!("function `{name}` in a constraint {what}");
    if !method.is_function || method.is_pure() {
        return Err(unsupported("that is not a function with a body"));
    }
    let Some(Some(result)) = &method.result else {
        return Err(unsupported("without a value"));
    };
    let shape = |ty: &Type| {
        casts::expr_type_from_type(&tree.functions, ty, env)
            .filter(|shape| (1..=64).contains(&shape.width))
            .map(|shape| (shape.width as u32, shape.signed))
    };
    let (width, signed) =
        shape(result).ok_or_else(|| unsupported("of a value wider than 64 bits"))?;
    let subroutine = native.subroutine(method.native).clone();
    let ports = match &subroutine.ports {
        Some(ports) => ports.clone(),
        None => subroutine
            .port_declarations
            .iter()
            .flat_map(|declaration| declaration.ports.iter().cloned())
            .collect(),
    };
    let mut formals = Vec::new();
    let mut body_inputs = inputs.clone();
    for (index, port) in ports.iter().enumerate() {
        if !matches!(port.direction, None | Some(Direction::Input)) || !port.dimensions.is_empty() {
            return Err(unsupported("with an argument other than a scalar input"));
        }
        let Some(port_name) = &port.name else {
            return Err(unsupported("with an unnamed argument"));
        };
        let port_name = instances::normalize_identifier(&port_name.spelling);
        if properties.iter().any(|property| property.name == port_name) {
            return Err(unsupported("with an argument named as a property"));
        }
        let ty = type_binding::procedural_type(
            port.data_type,
            native,
            tree,
            session,
            env,
            &TypeAliases::default(),
        )
        .map_err(|error| error.to_string())?
        .ok_or_else(|| unsupported("with an argument of an unknown type"))?;
        let (width, signed) =
            shape(&ty).ok_or_else(|| unsupported("with an argument wider than 64 bits"))?;
        body_inputs.insert(port_name, (FORMAL_INPUTS + index, width, signed));
        formals.push((width, signed));
    }
    if arguments.len() != formals.len()
        || arguments
            .iter()
            .any(|argument| argument.name.is_some() || argument.value.is_none())
    {
        return Err(unsupported(
            "called without one positional argument for each formal",
        ));
    }
    // One statement gives the value: `return value;` or `name = value;`.
    let value = match subroutine.items.as_slice() {
        [statement] => match &native.statement(*statement).kind {
            StatementKind::Return(Some(value)) => *value,
            StatementKind::Assignment {
                left,
                operator,
                right,
                nonblocking: false,
                timing: None,
                ..
            } if operator == "="
                && matches!(&native.expression(*left).kind,
                    ExprKind::Name(target) if instances::normalize_identifier(&target.spelling) == name) =>
            {
                *right
            }
            _ => return Err(unsupported("whose body is not one return of its value")),
        },
        _ => return Err(unsupported("whose body is not one return of its value")),
    };
    let body = bind_inputs(value, tree, session, properties, names, env, &body_inputs)?;
    if body.calls() {
        return Err(unsupported("that calls a function"));
    }
    let body = ConstraintExpr::Resize {
        value: Box::new(formal_arguments(body)),
        width,
        signed,
    };
    Ok(FunctionCall {
        arguments: arguments
            .iter()
            .filter_map(|argument| argument.value)
            .collect(),
        formals,
        body,
        width,
        signed,
    })
}

/// `expr` with each input that stands for a formal (see [`FORMAL_INPUTS`])
/// the argument of the formal.
fn formal_arguments(expr: ConstraintExpr) -> ConstraintExpr {
    let boxed = |value: ConstraintExpr| Box::new(formal_arguments(value));
    match expr {
        ConstraintExpr::Input { index, .. } if index >= FORMAL_INPUTS => {
            ConstraintExpr::Argument((index - FORMAL_INPUTS) as u32)
        }
        ConstraintExpr::Resize {
            value,
            width,
            signed,
        } => ConstraintExpr::Resize {
            value: boxed(*value),
            width,
            signed,
        },
        ConstraintExpr::Unary { op, value } => ConstraintExpr::Unary {
            op,
            value: boxed(*value),
        },
        ConstraintExpr::Binary {
            op,
            left,
            right,
            signed,
        } => ConstraintExpr::Binary {
            op,
            left: boxed(*left),
            right: boxed(*right),
            signed,
        },
        expr => expr,
    }
}

/// The class handle `id` names in a constraint, `null` or a handle
/// property, as an unsigned integer; `None` for another expression.
fn handle_operand(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    properties: &[Property],
    names: &Names<'_>,
    env: &ConstEnv,
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
        ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            match instance(&name, properties, names) {
                Some((_, property)) if property.r#type.class().is_some() => {
                    visible(property, tree, session, env)?;
                    Some(leaf(ConstraintExpr::Handle(property.field.offset)))
                }
                _ => None,
            }
        }
        _ => None,
    })
}

/// The arrays of the program, by index, that constraint `expr` loops over.
pub(in crate::ast) fn looped_arrays(expr: &ConstraintExpr, found: &mut Vec<usize>) {
    match expr {
        ConstraintExpr::Foreach { array, body } => {
            found.push(*array);
            looped_arrays(body, found);
        }
        ConstraintExpr::Enabled { value, .. }
        | ConstraintExpr::Soft { value, .. }
        | ConstraintExpr::Resize { value, .. }
        | ConstraintExpr::Unary { value, .. } => looped_arrays(value, found),
        ConstraintExpr::DisableSoft { .. } => {}
        ConstraintExpr::Binary { left, right, .. } => {
            looped_arrays(left, found);
            looped_arrays(right, found);
        }
        ConstraintExpr::Call { args, body } => {
            for arg in args {
                looped_arrays(arg, found);
            }
            looped_arrays(body, found);
        }
        ConstraintExpr::Literal(..)
        | ConstraintExpr::Field(_)
        | ConstraintExpr::Handle(_)
        | ConstraintExpr::Argument(_)
        | ConstraintExpr::Input { .. }
        | ConstraintExpr::Size(_)
        | ConstraintExpr::Index { .. }
        | ConstraintExpr::Static(_)
        | ConstraintExpr::Solved(_)
        | ConstraintExpr::Element { .. } => {}
    }
}

/// The array whose size member `id` is, as `a.size`.
fn array_size(id: ExprId, native: &celox_sv_syntax::Declarations) -> Option<String> {
    let ExprKind::Member {
        expression,
        separator,
        name,
    } = &native.expression(id).kind
    else {
        return None;
    };
    let ExprKind::Name(array) = &native.expression(*expression).kind else {
        return None;
    };
    (separator == "." && name.spelling == "size")
        .then(|| instances::normalize_identifier(&array.spelling))
}

/// The size of array property `name` among those of the program.
fn size(name: String, properties: &[Property], names: &Names<'_>) -> Result<Typed, String> {
    let (index, _) = instance(&name, properties, names)
        .filter(|(_, property)| property.sequence())
        .ok_or_else(|| format!("constraint variable `{name}.size`"))?;
    let array = properties[..index]
        .iter()
        .filter(|property| property.sequence())
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

/// Whether the solver holds a value of type `ty`, by its semantic type: a
/// 2-state integral one of at most 64 bits. Every other kind of type is
/// named here, so that a new one has to say whether the solver holds it.
pub(in crate::ast) fn solver_type(
    ty: &Type,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Result<(), String> {
    use super::super::semantic_types::{TyData, of_type};
    let id = of_type(&mut session.types, &tree.functions, ty, env)
        .ok_or("a value of a type a constraint does not take")?;
    match session.types.data(id) {
        TyData::Packed(packed) if packed.four_state => {
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
        && !(container.element.is_enum && container.element.enum_domain.is_none())
        && solver_type(&container.element, tree, session, env).is_ok()
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
    inputs: &HashMap<String, (usize, u32, bool)>,
) -> Result<ConstraintExpr, String> {
    #[allow(clippy::too_many_arguments)]
    fn expression(
        id: ExprId,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        properties: &[Property],
        names: &Names<'_>,
        env: &ConstEnv,
        inputs: &HashMap<String, (usize, u32, bool)>,
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
        if let Some(element) = element(id, tree, session, properties, env, loops)? {
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
            && let Some(a) = handle_operand(*left, tree, session, properties, names, env)?
            && let Some(b) = handle_operand(*right, tree, session, properties, names, env)?
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
                let ExprKind::Name(name) = &native.expression(*array).kind else {
                    return Err("constraint foreach over an array other than a property".into());
                };
                let name = instances::normalize_identifier(&name.spelling);
                let [Some(variable)] = variables.as_slice() else {
                    return Err("constraint foreach over more than one dimension".into());
                };
                let (index, property) = instance(&name, properties, names)
                    .filter(|(_, property)| property.sequence())
                    .ok_or_else(|| format!("constraint foreach over `{name}`"))?;
                if property.rand || property.randc {
                    return Err("constraint foreach over a rand array".into());
                }
                visible(property, tree, session, env)?;
                let array = properties[..index]
                    .iter()
                    .filter(|property| property.sequence())
                    .count();
                let mut inner = loops.to_vec();
                inner.push(Loop {
                    variable: instances::normalize_identifier(&variable.spelling),
                    array: name,
                    element: property
                        .r#type
                        .container
                        .as_ref()
                        .expect("a dynamic array or queue")
                        .clone(),
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
            ExprKind::Name(name) => {
                let name = instances::normalize_identifier(&name.spelling);
                let member = super::member(&name, names.lineage, properties, names.statics);
                if let Some(super::Member::Static(index)) = member {
                    // A static property of the class (18.5): the variable
                    // of the rand one the call solves, the value of another.
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
                    solver_type(&property.r#type, tree, session, env)?;
                    return Ok(if property.rand {
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
                    });
                }
                if let Some(super::Member::Instance(index)) = member {
                    let property = &properties[index];
                    visible(property, tree, session, env)?;
                    solver_holds(property, tree, session, env)?;
                    // The field of the property among those of the program.
                    let field = properties[..index]
                        .iter()
                        .filter(|property| constrainable(property))
                        .count();
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Field(field)),
                        width: property.field.width,
                        signed: property.field.signed,
                    }
                } else if let Some(&(index, width, signed)) = inputs.get(&name) {
                    Typed {
                        kind: Kind::Leaf(ConstraintExpr::Input { index, width }),
                        width,
                        signed,
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
            // The size of a dynamic array or queue (IEEE 1800-2023 7.5.2,
            // 7.10.2.1, 18.4), with or without the parentheses.
            ExprKind::Call {
                expression,
                arguments,
            } if arguments.is_empty() && array_size(*expression, native).is_some() => {
                return size(array_size(*expression, native).unwrap(), properties, names);
            }
            ExprKind::Member { .. } if array_size(id, native).is_some() => {
                return size(array_size(id, native).unwrap(), properties, names);
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
                let b = recurse(session, budget, *right)?;
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
            ExprKind::Call {
                expression: callee,
                arguments,
            } => {
                let call = function_call(
                    *callee, arguments, tree, session, properties, names, env, inputs,
                )?;
                // Its arguments, bound where the call is.
                let mut args = Vec::with_capacity(call.formals.len());
                for (argument, &(width, signed)) in call.arguments.iter().zip(&call.formals) {
                    let value = recurse(session, budget, *argument)?;
                    args.push(emit(sized(value, width, signed), None));
                }
                Typed {
                    kind: Kind::Leaf(ConstraintExpr::Call {
                        args,
                        body: Box::new(call.body),
                    }),
                    width: call.width,
                    signed: call.signed,
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
            | ExprKind::Set(_)
            | ExprKind::Streaming { .. }
            | ExprKind::StreamSelect { .. } => {
                return Err("unsupported scalar constraint expression".into());
            }
            ExprKind::SoftConstraint { .. } | ExprKind::DisableSoft { .. } => {
                return Err("soft constraint in a constraint loop".into());
            }
        })
    }
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
        ExprKind::SoftConstraint { expression } => Ok(ConstraintExpr::Soft {
            priority: 0,
            value: Box::new(bound(expression)?),
        }),
        ExprKind::DisableSoft { variable } => match bound(variable)? {
            variable @ (ConstraintExpr::Field(_)
            | ConstraintExpr::Static(_)
            | ConstraintExpr::Size(_)) => Ok(ConstraintExpr::DisableSoft {
                priority: 0,
                variable: Box::new(variable),
            }),
            _ => Err("disable soft of other than a random property or array size".into()),
        },
        _ => bound(id),
    }
}

/// The soft constraint or `disable soft` `constraint` with priority
/// `priority` (IEEE 1800-2023 18.5.13.1); another constraint as it is.
pub(in crate::ast) fn prioritize(constraint: ConstraintExpr, priority: u64) -> ConstraintExpr {
    match constraint {
        ConstraintExpr::Soft { value, .. } => ConstraintExpr::Soft { priority, value },
        ConstraintExpr::DisableSoft { variable, .. } => {
            ConstraintExpr::DisableSoft { priority, variable }
        }
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
            if property.randc && property.r#type.class().is_some() {
                return Err(unsupported("a randc object handle"));
            }
            if property.rand_object() {
                handles.push(property.field.offset);
                continue;
            }
            if property.sequence() {
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
            // (IEEE 1800-2023 18.4.2), kept for at most 256 values.
            if property.randc && property.cyclic_domain_size().is_none() {
                return Err(unsupported(
                    "constraints on a randc property of more than 256 values",
                ));
            }
            // A random enumeration takes only the values of its type (18.3).
            let mask = u64::MAX >> (64 - property.field.width);
            let domain = property
                .r#type
                .enum_domain
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
            });
        }
        // The slots of the static rand properties follow (see
        // `ConstraintExpr::Static`).
        let first = fields.len();
        for slot in &self.static_slots {
            solver_type(&slot.r#type, tree, session, env).map_err(unsupported)?;
            let mask = u64::MAX >> (64 - slot.field.width);
            fields.push(Field {
                offset: slot.field.offset,
                width: slot.field.width,
                random: true,
                domain: slot
                    .r#type
                    .enum_domain
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
        })
    }
}

/// Whether the solver can randomize dynamic array or queue property
/// `property`: one of 2-state integral elements of at most 64 bits.
pub(in crate::ast) fn random_array(
    property: &Property,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> bool {
    property.sequence()
        && property
            .r#type
            .container
            .as_ref()
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
    let container = property
        .r#type
        .container
        .as_ref()
        .expect("a dynamic array or queue");
    let layout = container.layout;
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
            .element
            .enum_domain
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
