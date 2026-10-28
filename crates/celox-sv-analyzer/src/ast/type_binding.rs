//! Native typedef identity and base-type binding into the existing semantic AST.
//!
//! Primitive and packed aggregate types consume native fields and bound syntax.
//! Datatypes, bounds and packed layouts bind from native handles without a
//! second syntax representation or fallback.

use super::instances::normalize_identifier;
use super::*;
use celox_sv_syntax::{DataTypeKind, TypeId};
#[cfg(test)]
mod aggregates;
pub(super) mod binding;
pub(super) use binding::{bind, expression_type};

/// Whether this declarator denotes a supported variable rather than a
/// constructed non-variable declaration (such as a covergroup instance).
pub(super) fn ordinary_variable(
    variable: &celox_sv_syntax::VariableDeclarator,
    data_type: celox_sv_syntax::TypeId,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    !variable.value.is_some_and(|id|matches!(&native.expression(id).kind,celox_sv_syntax::ExprKind::Name(name) if name.spelling=="new")) || names_class(data_type, native)
}

/// Whether `data_type` names a class declared in the source or a built-in
/// class constructed with `new` (IEEE 1800-2023 15.3).
pub(super) fn names_class(
    data_type: celox_sv_syntax::TypeId,
    native: &celox_sv_syntax::Declarations,
) -> bool {
    let celox_sv_syntax::DataTypeKind::Named(segments) = &native.data_type(data_type).kind else {
        return false;
    };
    let segment = match segments.as_slice() {
        [segment] => segment,
        [std, segment] if std.name.spelling == "std" => segment,
        _ => return false,
    };
    let name = normalize_identifier(&segment.name.spelling);
    if name == "semaphore" {
        return true;
    }
    if segments.len() > 1 {
        return false;
    }
    native
        .classes
        .iter()
        .any(|class| normalize_identifier(&class.name.spelling) == name)
}

pub(super) fn port_direction(direction: celox_sv_syntax::Direction) -> PortDirection {
    match direction {
        celox_sv_syntax::Direction::Input => PortDirection::Input,
        celox_sv_syntax::Direction::Output => PortDirection::Output,
        celox_sv_syntax::Direction::Inout => PortDirection::Inout,
        celox_sv_syntax::Direction::Ref => PortDirection::Ref,
    }
}

pub(super) fn port_type(
    port: &celox_sv_syntax::Port,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let Some(id) = port.data_type else {
        return Ok(None);
    };
    let ty = bind(id, tree, &context)?;
    if ty.as_ref().is_some_and(|ty| ty.unpacked_struct().is_some()) {
        return Err(unsupported("port of unpacked structure type"));
    }
    Ok(ty)
}

/// Formal/return implicit types use scalar logic rather than an unsized local.
pub(super) fn implicit_subroutine_type(
    id: TypeId,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Type> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let data = tree.declarations().data_type(id);
    let mut ty = Type::new(TypeKind::Logic);
    ty.is_signed = data.signed.unwrap_or(false);
    ty.packed_ranges = binding::packed_ranges(&data.packed_dimensions, tree, &context)?;
    Ok(ty)
}
pub(super) fn local_signals(
    declaration: &celox_sv_syntax::VariableDeclaration,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    selected_name: Option<&str>,
) -> Converted<Vec<Signal>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let Some(base) = bind(declaration.data_type, tree, &context)? else {
        return Err(unsupported("unsupported internal data type"));
    };
    let mut signals = Vec::new();
    for variable in &declaration.variables {
        if !ordinary_variable(variable, declaration.data_type, tree.declarations()) {
            continue;
        }
        let name = normalize_identifier(&variable.name.spelling);
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let ty = containers::with_dimensions(base.clone(), &variable.dimensions, tree, &context)?;
        if declaration.constant && ty.container.is_some() {
            return Err(unsupported("const dynamic container"));
        }
        let mut signal = Signal::new(name, ty);
        signal.is_const = declaration.constant;
        signals.push(signal);
    }
    Ok(signals)
}

/// Ordinary design-unit nets use authored native type and dimension operands.
/// Native lexical/active item membership supplies declaration identity.
pub(super) fn net_signals(
    declaration: &celox_sv_syntax::NetDeclaration,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    selected_name: Option<&str>,
) -> Converted<Vec<Signal>> {
    if declaration.net_type == "interconnect" {
        return Err(unsupported("interconnect net declaration"));
    }
    if matches!(
        tree.declarations().data_type(declaration.data_type).kind,
        DataTypeKind::Enum { .. }
    ) {
        return Err(unsupported("unsupported net data type"));
    }
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let Some(base) = bind(declaration.data_type, tree, &context)? else {
        return Err(unsupported("unsupported net data type"));
    };
    let mut signals = Vec::new();
    for variable in &declaration.variables {
        let name = normalize_identifier(&variable.name.spelling);
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let ty = containers::with_dimensions(base.clone(), &variable.dimensions, tree, &context)?;
        if ty.container.is_some() {
            return Err(unsupported("dynamic container net"));
        }
        signals
            .push(Signal::new_net(name, ty).with_unresolved_net(declaration.net_type == "uwire"));
    }
    Ok(signals)
}

/// Value/type ambiguity is retained in authored parameter syntax. A named
/// packed type may be an expression-shaped name/select until alias resolution.
pub(super) fn argument_type(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    if let celox_sv_syntax::ExprKind::Type(id) = tree.declarations().expression(root).kind {
        return bind(id, tree, &context);
    }
    let Some((name, dimensions)) = argument_alias(root, tree.declarations()) else {
        return Ok(None);
    };
    let Some(base) = aliases.get(&name) else {
        return Ok(None);
    };
    let ranges = binding::packed_ranges(&dimensions, tree, &context)?;
    Ok(Some(types::enclose_aliased_packed_type(
        base.clone(),
        ranges,
    )))
}

pub(super) fn argument_alias(
    mut root: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> Option<(String, Vec<celox_sv_syntax::Dimension>)> {
    use celox_sv_syntax::ExprKind;
    let mut dimensions = Vec::new();
    while let ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(root).kind
    {
        dimensions.push((**dimension).clone());
        root = *expression;
    }
    dimensions.reverse();
    let mut suffixes = Vec::new();
    let mut name = loop {
        match &native.expression(root).kind {
            ExprKind::Name(name) => break normalize_identifier(&name.spelling),
            ExprKind::Member {
                expression,
                separator,
                name,
            } => {
                suffixes.push((separator, &name.spelling));
                root = *expression;
            }
            _ => return None,
        }
    };
    for (separator, suffix) in suffixes.into_iter().rev() {
        name.push_str(separator);
        name.push_str(&normalize_identifier(suffix));
    }
    Some((name, dimensions))
}

/// Procedural declarations share native type jobs and semantic layouts with typedefs.
pub(super) fn procedural_type(
    id: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    binding::bind_in(id, native, tree, &context)
}

/// Enum constant bases use the declared native type and bound handles.
pub(super) fn enum_constant_base(
    id: TypeId,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    bind(id, tree, &context)
}

pub(super) fn alias_base_type(
    id: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    defer_class_defaults: bool,
) -> Converted<Option<Type>> {
    // Typedef type-reference inference is outside the supported alias path.
    if matches!(native.data_type(id).kind, DataTypeKind::TypeOf(_)) {
        return Ok(None);
    }
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    binding::bind_alias_in(id, native, tree, &context, defer_class_defaults)
}

/// Builtin integral identity/width without syntax or aggregate layout.
pub(super) fn builtin_type(name: &str, signing: Option<bool>) -> Option<Type> {
    let (kind, width, signed) = match name {
        "real" | "realtime" => (TypeKind::Real, Some(64), true),
        "shortreal" => (TypeKind::Shortreal, Some(32), true),
        // A heap handle; see `celox_design::HANDLE_WIDTH`.
        "string" => (TypeKind::String, Some(32), false),
        "bit" => (TypeKind::Bit, None, false),
        "logic" => (TypeKind::Logic, None, false),
        "reg" => (TypeKind::Reg, None, false),
        "byte" => (TypeKind::Bit, Some(8), true),
        "shortint" => (TypeKind::Bit, Some(16), true),
        "int" => (TypeKind::Bit, Some(32), true),
        "longint" => (TypeKind::Bit, Some(64), true),
        "integer" => (TypeKind::Logic, Some(32), true),
        "time" => (TypeKind::Logic, Some(64), false),
        // A named event is a handle (IEEE 1800-2023 15.5).
        "event" => return Some(super::classes::handle(super::events::CLASS)),
        _ => return None,
    };
    let mut ty = Type::new(kind);
    ty.is_signed = signing.unwrap_or(signed);
    let keyword = match name {
        "byte" => Some("byte"),
        "shortint" => Some("shortint"),
        "int" => Some("int"),
        "longint" => Some("longint"),
        "integer" => Some("integer"),
        "time" => Some("time"),
        _ => None,
    };
    if let Some(keyword) = keyword {
        ty.spelling = type_names::Spelling::keyword(keyword);
    }
    if let Some(width) = width {
        ty.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal((width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        ));
    }
    Some(ty)
}

/// An authored alias variable can be represented as a net-type-identifier in
/// the old grammar. Its native declaration remains a variable, resolved here.
pub(super) fn alias_signals(
    declaration: &celox_sv_syntax::VariableDeclaration,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    selected_name: Option<&str>,
) -> Converted<Vec<Signal>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let Some(base) = bind(declaration.data_type, tree, &context)? else {
        return Ok(Vec::new());
    };
    let mut signals = Vec::new();
    for variable in &declaration.variables {
        let name = normalize_identifier(&variable.name.spelling);
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let ty = containers::with_dimensions(base.clone(), &variable.dimensions, tree, &context)?;
        signals.push(Signal::new(name, ty));
    }
    Ok(signals)
}

mod subroutines;
pub(super) use subroutines::subroutine_formals;
