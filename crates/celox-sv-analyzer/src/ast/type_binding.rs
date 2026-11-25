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
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    declarations: &Arc<types::TypeDeclarations>,
) -> Converted<Option<Type>> {
    let mut context = PackedDimensions::new(HashMap::default(), env, aliases);
    context.type_declarations = Some(declarations.clone());
    let Some(id) = port.data_type else {
        return Ok(None);
    };
    bind(id, tree, session, &context)
}

/// Whether the range of the port declaration of a non-ANSI port, which its
/// net or variable declaration of type `object_type` repeats in other
/// words, has the bounds of the port's type `ty`, or the width of a type
/// without a range (IEEE 1800-2023 23.2.2.1).
pub(super) fn declared_port_range_matches(
    declared_type: Option<celox_sv_syntax::TypeId>,
    object_type: Option<celox_sv_syntax::TypeId>,
    ty: &Type,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<bool> {
    let Some(id) = declared_type else {
        return Ok(true);
    };
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let data = tree.declarations().data_type(id);
    let declared = binding::packed_ranges(&data.packed_dimensions, tree, session, &context)?;
    let bounds = |range: &PackedRange| {
        constants::eval_ast_const_expr(&tree.functions, range.left(), env).zip(
            constants::eval_ast_const_expr(&tree.functions, range.right(), env),
        )
    };
    // A type keyword or name without a range has the width of the port
    // declaration's range; a real type has none.
    let object_ranged = object_type.is_some_and(|id| {
        !tree
            .declarations()
            .data_type(id)
            .packed_dimensions
            .is_empty()
    });
    if !object_ranged {
        let Some(width) = declared.iter().try_fold(1u128, |width, range| {
            let (left, right) = bounds(range)?;
            width.checked_mul(left.abs_diff(right) + 1)
        }) else {
            return Ok(true);
        };
        let bits =
            semantic_types::of_type(&mut session.types, &tree.functions, ty, env).map(|id| {
                match session.types.data(id) {
                    semantic_types::TyData::Packed(packed) => Some(packed.bits()),
                    _ => None,
                }
            });
        return Ok(match bits {
            Some(bits) => bits == Some(width),
            None => true,
        });
    }
    Ok(declared.len() == ty.packed_ranges.len()
        && declared
            .iter()
            .zip(&ty.packed_ranges)
            .all(
                |(declared, range)| match (bounds(declared), bounds(range)) {
                    (Some(declared), Some(range)) => declared == range,
                    _ => declared == range,
                },
            ))
}

/// Formal/return implicit types use scalar logic rather than an unsized local.
pub(super) fn implicit_subroutine_type(
    id: TypeId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: impl ToShared<i128, ConstEnv>,
    aliases: impl ToShared<Type, TypeAliases>,
) -> Converted<Type> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    let data = tree.declarations().data_type(id);
    let mut ty = Type::new(TypeKind::Logic);
    ty.is_signed = data.signed.unwrap_or(false);
    ty.packed_ranges = binding::packed_ranges(&data.packed_dimensions, tree, session, &context)?;
    semantic_types::bind_declared(
        &mut session.types,
        &tree.functions,
        &mut ty,
        &context.const_env,
    );
    Ok(ty)
}
/// The signals that `declaration` declares. A type operator applied to an
/// expression names variables of `scope`, the declarations before it
/// (IEEE 1800-2023 6.23).
pub(super) fn local_signals(
    declaration: &celox_sv_syntax::VariableDeclaration,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    declarations: Option<&Arc<types::TypeDeclarations>>,
    selected_name: Option<&str>,
    scope: Option<&PackedDimensions>,
) -> Converted<Vec<Signal>> {
    // A type reference declares a variable only after `var` (6.23).
    if matches!(
        tree.declarations().data_type(declaration.data_type).kind,
        DataTypeKind::TypeOf(_)
    ) && !declaration.var
    {
        return Err(unsupported(
            "variable declaration with a type reference but without `var` (IEEE 1800-2023 6.23)",
        ));
    }
    let context = match scope {
        Some(scope)
            if matches!(
                tree.declarations().data_type(declaration.data_type).kind,
                DataTypeKind::TypeOf(_)
            ) =>
        {
            scope.clone()
        }
        _ => {
            let mut context = PackedDimensions::new(HashMap::default(), env, aliases);
            context.type_declarations = declarations.cloned();
            context
        }
    };
    let Some(base) = bind(declaration.data_type, tree, session, &context)? else {
        // Ports, and variables of a generate block, are not in the scope of
        // a type operator.
        if matches!(
            tree.declarations().data_type(declaration.data_type).kind,
            DataTypeKind::TypeOf(_)
        ) {
            return Err(unsupported(
                "type operator applied to an expression whose type is not known at its declaration (one reading a port, or any expression in a generate block)",
            ));
        }
        return Err(unsupported("unsupported internal data type"));
    };
    let mut signals = Vec::new();
    for variable in &declaration.variables {
        let name = normalize_identifier(&variable.name.spelling);
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let ty = containers::with_dimensions(
            base.clone(),
            &variable.dimensions,
            tree,
            session,
            &context,
        )?;
        if declaration.constant && ty.container().is_some() {
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
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    declarations: Option<&Arc<types::TypeDeclarations>>,
    selected_name: Option<&str>,
) -> Converted<Vec<Signal>> {
    // A generic interconnect net (6.6.8) has no data type of its own: the
    // authored implicit type only states signing and packed dimensions, and
    // the connections determine the rest at elaboration (23.3.3.7.1). Its
    // `NetResolution::Interconnect` marker carries that through the IR.
    if declaration.net_type == "interconnect" {
        // An interconnect net has no data type: only the signing and
        // packed dimensions of an implicit data type may be authored
        // (6.6.8, Syntax 6-2).
        if !matches!(
            tree.declarations().data_type(declaration.data_type).kind,
            DataTypeKind::Implicit
        ) {
            return Err(unsupported("interconnect net with a data type"));
        }
        if declaration
            .variables
            .iter()
            .any(|variable| variable.value.is_some())
        {
            return Err(unsupported("interconnect net declaration assignment"));
        }
    }
    if matches!(
        tree.declarations().data_type(declaration.data_type).kind,
        DataTypeKind::Enum { .. }
    ) {
        return Err(unsupported("unsupported net data type"));
    }
    let mut context = PackedDimensions::new(HashMap::default(), env, aliases);
    context.type_declarations = declarations.cloned();
    let Some(base) = bind(declaration.data_type, tree, session, &context)? else {
        return Err(unsupported("unsupported net data type"));
    };
    // A net has a four-state integral type or an unpacked aggregate of
    // such (6.7.1): no string, handle or chandle.
    let illegal = match base.kind {
        TypeKind::String => Some("type `string`"),
        TypeKind::Object | TypeKind::Process | TypeKind::Event => Some("a handle type"),
        TypeKind::Chandle => Some("type `chandle`"),
        _ => None,
    };
    if let Some(data_type) = illegal
        && let Some(variable) = declaration.variables.first()
    {
        return Err(AnalyzerError::IllegalNetDataType {
            name: normalize_identifier(&variable.name.spelling),
            data_type,
        });
    }
    let mut signals = Vec::new();
    for variable in &declaration.variables {
        let name = normalize_identifier(&variable.name.spelling);
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let ty = containers::with_dimensions(
            base.clone(),
            &variable.dimensions,
            tree,
            session,
            &context,
        )?;
        if ty.container().is_some() {
            return Err(unsupported("dynamic container net"));
        }
        signals.push(net_signal(name, ty, &declaration.net_type));
    }
    Ok(signals)
}

/// The net `name` of type `ty` and of net type `net_type`.
fn net_signal(name: String, ty: Type, net_type: &str) -> Signal {
    let drive = match net_type {
        "tri0" => NetDrive::Pull(false),
        "tri1" => NetDrive::Pull(true),
        "supply0" => NetDrive::Supply(false),
        "supply1" => NetDrive::Supply(true),
        _ => NetDrive::None,
    };
    Signal::new_net(name, ty)
        .with_unresolved_net(net_type == "uwire")
        .with_net_resolution(NetResolution::of(net_type))
        .with_net_drive(drive)
}

/// The implicit scalar net `name` of net type `net_type` (IEEE 1800-2023
/// 6.10), as a declaration `net_type name;` declares it.
pub(super) fn implicit_net(name: String, net_type: &str) -> Signal {
    net_signal(name, Type::implicit(), net_type)
}

/// Value/type ambiguity is retained in authored parameter syntax. A named
/// packed type may be an expression-shaped name/select until alias resolution.
pub(super) fn argument_type(
    root: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    if let celox_sv_syntax::ExprKind::Type(id) = tree.declarations().expression(root).kind {
        return bind(id, tree, session, &context);
    }
    let Some((name, dimensions)) = argument_alias(root, tree.declarations()) else {
        return Ok(None);
    };
    let Some(base) = aliases.get(&name) else {
        return Ok(None);
    };
    let ranges = binding::packed_ranges(&dimensions, tree, session, &context)?;
    // A chandle is no packed array element (IEEE 1800-2023 6.14).
    if base.is_chandle() && !ranges.is_empty() {
        return types::packed_chandles();
    }
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
    session: &mut Session,
    env: impl ToShared<i128, ConstEnv>,
    aliases: impl ToShared<Type, TypeAliases>,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    binding::bind_in(id, native, tree, session, &context)
}

/// Enum constant bases use the declared native type and bound handles.
pub(super) fn enum_constant_base(
    id: TypeId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Option<Type>> {
    let context = PackedDimensions::new(HashMap::default(), env, aliases);
    bind(id, tree, session, &context)
}

pub(super) fn alias_base_type(
    id: TypeId,
    native: &celox_sv_syntax::Declarations,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: impl ToShared<i128, ConstEnv>,
    aliases: impl ToShared<Type, TypeAliases>,
    declarations: Option<&Arc<types::TypeDeclarations>>,
    defer_class_defaults: bool,
) -> Converted<Option<Type>> {
    // A typedef of a type reference (IEEE 1800-2023 6.23) is not bound,
    // rather than skipped as if it named nothing.
    if matches!(native.data_type(id).kind, DataTypeKind::TypeOf(_)) {
        return Err(unsupported("typedef of a type reference"));
    }
    let mut context = PackedDimensions::new(HashMap::default(), env, aliases);
    context.type_declarations = declarations.cloned();
    binding::bind_alias_in(id, native, tree, session, &context, defer_class_defaults)
}

/// [`builtin_type`], or the type of named events (IEEE 1800-2023 15.5), a
/// handle whose semantic type names its built-in class in `table`.
pub(super) fn builtin_type_in(
    table: &mut semantic_types::TyTable,
    name: &str,
    signing: Option<bool>,
) -> Option<Type> {
    if name == "event" {
        return Some(super::classes::handle(table, super::events::CLASS));
    }
    builtin_type(name, signing)
}

/// Builtin integral identity/width without syntax or aggregate layout. The
/// type of named events is a class handle, which [`builtin_type_in`] binds.
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
        "chandle" => (TypeKind::Chandle, Some(64), false),
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
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
    declarations: Option<&Arc<types::TypeDeclarations>>,
    selected_name: Option<&str>,
) -> Converted<Vec<Signal>> {
    let mut context = PackedDimensions::new(HashMap::default(), env, aliases);
    context.type_declarations = declarations.cloned();
    let Some(base) = bind(declaration.data_type, tree, session, &context)? else {
        return Ok(Vec::new());
    };
    let mut signals = Vec::new();
    for variable in &declaration.variables {
        let name = normalize_identifier(&variable.name.spelling);
        if selected_name.is_some_and(|selected| selected != name) {
            continue;
        }
        let ty = containers::with_dimensions(
            base.clone(),
            &variable.dimensions,
            tree,
            session,
            &context,
        )?;
        signals.push(Signal::new(name, ty));
    }
    Ok(signals)
}

mod subroutines;
pub(super) use subroutines::{port_formals, subroutine_formals};
