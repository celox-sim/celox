//! `$typename` (IEEE 1800-2023 20.6.1): the string that represents the
//! resolved type of a data type or of an expression.
//!
//! The string is built from the semantic type ([`TyData`]) and the
//! declarations it names ([`semantic_types::DeclData`]): typedefs are
//! already resolved, a signing equal to the default is not written, ranges
//! are written as `[left:right]` decimal numbers, an unpacked dimension
//! follows the `$` that stands for the name of the array, and an
//! enumeration, struct or union is written with its members and the
//! scope-qualified name of its typedef, which its declaration records. A
//! type this cannot spell exactly, such as an anonymous enumeration, is
//! rejected rather than spelled differently.
use super::*;
use semantic_types::{ArrayKind, Decl, DeclId, PackedElement, TyData, TyId};

/// The declaration that names an enumeration, struct or union type, and how
/// `$typename` spells a type where its structure does not tell. Type
/// equality does not compare it: the declaration tells a type apart from
/// another of the same structure (IEEE 1800-2023 6.19, 6.22.1) where a class
/// specialization is keyed by its type arguments (8.25), and `int` matches
/// `bit signed [31:0]` (6.22.1) although `$typename` tells them apart. It is
/// one pointer, as types are held on deeply recursive binding frames.
#[derive(Debug, Clone, Default)]
pub(super) struct Spelling(Option<Arc<SpellingParts>>);

#[derive(Debug, Clone, Default)]
struct SpellingParts {
    /// The identity of the typedef that names an enumeration, struct or
    /// union type.
    declaration: Option<Arc<str>>,
    /// The integer type with a predefined width (6.11) that declared the
    /// type, which spells it while the type keeps its one range.
    keyword: Option<&'static str>,
    /// `struct` or `union` of a packed aggregate other than a soft or
    /// tagged union.
    aggregate: Option<&'static str>,
    /// The name of the typedef of an enumeration, struct or union.
    name: Option<TypedefName>,
    /// The elaborated scope that declares the enumeration, struct or union
    /// type: an analysis of a module, interface or program, which each
    /// instance with other parameter values has (6.22). `None` for a
    /// package, a compilation unit or a class specialization, whose
    /// declarations are named apart already.
    scope: Option<Arc<str>>,
}

impl Spelling {
    pub(super) fn keyword(keyword: &'static str) -> Self {
        Self(Some(Arc::new(SpellingParts {
            keyword: Some(keyword),
            ..SpellingParts::default()
        })))
    }

    pub(super) fn aggregate(aggregate: &'static str) -> Self {
        Self(Some(Arc::new(SpellingParts {
            aggregate: Some(aggregate),
            ..SpellingParts::default()
        })))
    }

    /// Name the type after the typedef whose identity is `declaration` and
    /// whose `$typename` name is `name`, declared in elaborated scope
    /// `scope`.
    pub(super) fn name(
        &mut self,
        declaration: Arc<str>,
        name: TypedefName,
        scope: Option<Arc<str>>,
    ) {
        self.name_spelled(declaration, Some(name), scope);
    }

    /// [`Self::name`] for a typedef whose `$typename` name may not be
    /// known.
    pub(super) fn name_spelled(
        &mut self,
        declaration: Arc<str>,
        name: Option<TypedefName>,
        scope: Option<Arc<str>>,
    ) {
        let mut parts = self.0.as_deref().cloned().unwrap_or_default();
        parts.declaration = Some(declaration);
        parts.name = name;
        parts.scope = scope;
        self.0 = Some(Arc::new(parts));
    }

    /// Record that elaborated scope `scope` declares the anonymous type.
    pub(super) fn declared_in(&mut self, scope: Arc<str>) {
        let mut parts = self.0.as_deref().cloned().unwrap_or_default();
        parts.scope = Some(scope);
        self.0 = Some(Arc::new(parts));
    }

    /// The identity of the typedef that names the type.
    pub(super) fn declaration(&self) -> Option<&str> {
        self.parts()?.declaration.as_deref()
    }

    /// The elaborated scope that declares the type.
    pub(super) fn scope(&self) -> Option<&str> {
        self.parts()?.scope.as_deref()
    }

    fn parts(&self) -> Option<&SpellingParts> {
        self.0.as_deref()
    }

    pub(super) fn keyword_of(&self) -> Option<&'static str> {
        self.parts()?.keyword
    }

    pub(super) fn aggregate_of(&self) -> Option<&'static str> {
        self.parts()?.aggregate
    }

    pub(super) fn name_of(&self) -> Option<&TypedefName> {
        self.parts()?.name.as_ref()
    }
}

impl PartialEq for Spelling {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Spelling {}

impl std::hash::Hash for Spelling {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}

/// The name of a typedef, prefixed with the scope that declares it (rule f).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TypedefName {
    /// Declared in a package (`p::t`), a module, interface or program
    /// (`m.t`, `m.g.t` in generate scope `g`) or the compilation unit
    /// (`$unit::t`).
    Scope(Arc<str>),
    /// Declared in specialization `class` of a class.
    Class { class: Arc<str>, name: Arc<str> },
}

/// Name `name` declared in `unit`, or in the compilation unit when `unit`
/// is `None`, prefixed with that scope (rule f).
pub(super) fn scoped_name(unit: Option<&celox_sv_syntax::Unit>, name: &str) -> String {
    match unit {
        Some(unit) => {
            let unit_name = instances::normalize_identifier(&unit.name.spelling);
            if unit.kind == celox_sv_syntax::UnitKind::Package {
                format!("{unit_name}::{name}")
            } else {
                format!("{unit_name}.{name}")
            }
        }
        None => format!("{}::{name}", packages::UNIT),
    }
}

/// The typedef that names the declared enumeration, struct or union element
/// type of `shape`.
pub(super) fn declaration_of(shape: &VariableDimensions) -> Option<String> {
    // The typedef names the element type, which selecting from an array
    // or changing its dimensions does not change.
    shape
        .declared
        .0
        .as_deref()?
        .spelling
        .declaration()
        .map(str::to_owned)
}

/// The declared type of the variable of `shape`, whose element type it
/// still names after the shape selects from it or changes its dimensions.
pub(super) fn declared_type(shape: &VariableDimensions) -> Option<&Type> {
    shape.declared.0.as_deref()
}

/// The declared type of a variable, for `$typename` of its name. Shape
/// equality does not compare it.
#[derive(Debug, Clone, Default)]
pub(super) struct Declared(Option<Arc<Type>>);

impl Declared {
    pub(super) fn new(r#type: &Type) -> Self {
        Self(Some(Arc::new(r#type.clone())))
    }

    /// The declared type of `shape`, whatever its dimensions.
    pub(super) fn of_shape(
        shape: &crate::ast::variable_dimensions::VariableShape,
    ) -> Option<&Type> {
        shape.declared.0.as_deref()
    }

    /// Whether `other` declares the same type, which shape equality does
    /// not compare.
    pub(super) fn same(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    /// Feed the declared type to `state`, which the `Hash` of a shape does
    /// not.
    pub(super) fn hash_type<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.0, state);
    }

    /// The declared type, if the shape still has its declared dimensions.
    fn of(shape: &VariableDimensions) -> Option<&Type> {
        let r#type = shape.declared.0.as_deref()?;
        (r#type.packed_ranges.len() == shape.packed.len()
            && r#type.unpacked_ranges.len() == shape.unpacked.len()
            && r#type.is_signed == shape.signed
            && r#type.container().is_some() == shape.container().is_some()
            && r#type.element.identity() == shape.element_identity())
        .then_some(r#type)
    }
}

impl PartialEq for Declared {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for Declared {}

impl std::hash::Hash for Declared {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {}
}

/// An actual parameter of a class specialization, as its `$typename`
/// writes it.
#[derive(Debug, Clone)]
pub(super) enum ClassArgument {
    Type(Type),
    /// The value of a value parameter, or `None` for one without an
    /// integral value.
    Value(Option<i128>),
}

fn unsupported_operand(what: impl std::fmt::Display) -> AnalyzerError {
    unsupported(format!("`$typename` of {what}"))
}

/// The string of a `$typename(argument)` call.
pub(super) fn query_string(
    argument: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<String> {
    let context = dimensions::query_context_for_operand(argument, tree, session, context)?;
    let r#type = operand_type(argument, tree, session, &context)?;
    type_name(&r#type, tree, session, &context.const_env)
}

/// The value of a `$typename(argument)` call: the string as a literal.
pub(super) fn query_value(
    argument: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<Expr> {
    let name = query_string(argument, tree, session, context)?;
    // A string, as the result of the enumeration method `name` is (6.19.5.6).
    Ok(expression_binding::string_call(
        system_functions::StringOp::FromBits,
        vec![Expr::Literal(string_literal(&name))],
    ))
}

/// The bits of string literal `text` (5.9).
fn string_literal(text: &str) -> String {
    let hex = text
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{}'h{hex}", text.len() * 8)
}

fn operand_type(
    argument: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Converted<Type> {
    let native = tree.declarations();
    let argument = expression_binding::ungroup(argument, native);
    // A hierarchical variable such as `g.v` of a generate block, or a type
    // name with packed dimensions, `t [3:0]`, which parses as a select.
    if let Some((name, dimensions)) = type_binding::argument_alias(argument, native)
        && (name.contains('.') || !dimensions.is_empty())
    {
        if let Some(shape) = context.get(&name) {
            if dimensions.is_empty() {
                return Declared::of(shape)
                    .cloned()
                    .ok_or_else(|| unsupported_operand(format!("`{name}`")));
            }
        } else if !dimensions.is_empty()
            && !context.const_env.contains_key(&name)
            && !context.parameter_values.contains_key(&name)
            && let Some(base) = match context.type_aliases.get(&name) {
                Some(base) => Some(base.clone()),
                // A type of a package or class, `C::T [3:0]`.
                None => classes::specializations::scoped_type(
                    tree,
                    session,
                    selected_base(argument, native),
                    context,
                )?,
            }
        {
            let ranges = type_binding::binding::packed_ranges(&dimensions, tree, session, context)?;
            return Ok(types::enclose_aliased_packed_type(base, ranges));
        }
    }
    match &native.expression(argument).kind {
        celox_sv_syntax::ExprKind::Type(id) => type_binding::bind(*id, tree, session, context)?
            .ok_or_else(|| unsupported_operand("this data type")),
        celox_sv_syntax::ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            if let Some(shape) = context.get(&name) {
                return Declared::of(shape)
                    .cloned()
                    .ok_or_else(|| unsupported_operand(format!("`{name}`")));
            }
            if context.const_env.contains_key(&name) || context.parameter_values.contains_key(&name)
            {
                return Err(unsupported_operand(format!("parameter `{name}`")));
            }
            if let Some((property, _)) =
                expression_binding::object_property(tree, session, argument, native, context)?
            {
                return Ok(property.r#type.clone());
            }
            if let Some(r#type) = context.type_aliases.get(&name) {
                return Ok(r#type.clone());
            }
            if let Some(r#type) = class_operand(tree, session, &name, context)? {
                return Ok(r#type);
            }
            Err(unsupported_operand(format!("`{name}`")))
        }
        celox_sv_syntax::ExprKind::Member { .. } => {
            if let Some((property, _)) =
                expression_binding::object_property(tree, session, argument, native, context)?
            {
                return Ok(property.r#type.clone());
            }
            // A static property, `C::v` or `h.v` (8.9).
            if let Some(property) = expression_binding::object_static_property_declaration(
                tree, session, argument, native, context,
            )? {
                return Ok(property.r#type);
            }
            // A type of a package or class, `p::t` or `C::T`.
            if let Some(r#type) =
                classes::specializations::scoped_type(tree, session, argument, context)?
            {
                return Ok(r#type);
            }
            // A class of a package, `p::C`.
            if let celox_sv_syntax::ExprKind::Member {
                expression,
                separator,
                name,
            } = &native.expression(argument).kind
                && separator == "::"
                && let celox_sv_syntax::ExprKind::Name(package) =
                    &native.expression(*expression).kind
                && let qualified = scope::qualified_name(
                    tree,
                    &instances::normalize_identifier(&package.spelling),
                    &instances::normalize_identifier(&name.spelling),
                )
                && let Some(r#type) = class_operand(tree, session, &qualified, context)?
            {
                return Ok(r#type);
            }
            Err(unsupported_operand("this member or hierarchical name"))
        }
        celox_sv_syntax::ExprKind::Parameterized { .. } => {
            expression_binding::named_class(tree, session, argument, native, context)?
                .map(|class| classes::handle(&mut session.types, &class))
                .ok_or_else(|| unsupported_operand("this class"))
        }
        _ => Err(unsupported_operand(
            "an expression other than a variable, property or data type",
        )),
    }
}

/// The handle type of the class `name` names, if it names one.
fn class_operand(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    let Some(class) = classes::resolve(tree, session, name) else {
        return Ok(None);
    };
    // A parameterized class named alone is its default specialization
    // (8.25).
    if classes::is_parameterized(tree, session, &class) {
        let class = classes::specializations::specialize(tree, session, &class, &[], context)?;
        return Ok(Some(classes::handle(&mut session.types, &class)));
    }
    Ok(classes::handle_type(tree, session, &class))
}

/// The expression that the selects of native expression `id` apply to.
fn selected_base(
    mut id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
) -> celox_sv_syntax::ExprId {
    while let celox_sv_syntax::ExprKind::Select { expression, .. } = native.expression(id).kind {
        id = expression;
    }
    id
}

/// The `$typename` string of `r#type`, with constants evaluated in `env`.
pub(super) fn type_name(
    r#type: &Type,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    // The type the declaration gave it, where it was bound.
    let id = semantic_types::of_declared(&mut session.types, &tree.functions, r#type, env)
        .ok_or_else(|| unsupported_operand("a type with a range that is not constant"))?;
    name_of(id, tree, session, env)
}

/// The `$typename` string of semantic type `id`: its element type, then
/// its unpacked dimensions, slowest varying first.
fn name_of(
    id: TyId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    let mut dimensions = String::new();
    let mut element = id;
    let mut container = false;
    while let TyData::Array {
        kind,
        element: inner,
    } = session.types.data(element).clone()
    {
        // 20.6.1 spells the dimensions of an array, not the arrays of a
        // variable-size dimension.
        if container {
            return Err(unsupported_operand("an array of containers"));
        }
        match kind {
            ArrayKind::Fixed(range) => {
                dimensions.push_str(&format!("[{}:{}]", range.left, range.right));
            }
            ArrayKind::Dynamic => dimensions.push_str("[]"),
            ArrayKind::Queue { bound: None } => dimensions.push_str("[$]"),
            ArrayKind::Queue { bound: Some(bound) } => {
                dimensions.push_str(&format!("[$:{bound}]"));
            }
            ArrayKind::Associative { key } => {
                dimensions.push_str(&format!("[{}]", name_of(key, tree, session, env)?));
            }
        }
        container = !matches!(kind, ArrayKind::Fixed(_));
        element = inner;
    }
    let mut name = element_name(element, tree, session, env)?;
    if !dimensions.is_empty() {
        // `$` stands for the name of the unpacked array (rule d).
        name.push('$');
        name.push_str(&dimensions);
    }
    Ok(name)
}

/// The name of semantic type `id`, an element of an unpacked array.
fn element_name(
    id: TyId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    match session.types.data(id).clone() {
        TyData::Packed(packed) => packed_name(&packed, tree, session, env),
        TyData::Real => Ok("real".to_owned()),
        TyData::Shortreal => Ok("shortreal".to_owned()),
        TyData::String => Ok("string".to_owned()),
        TyData::Chandle => Ok("chandle".to_owned()),
        // An event is a handle of a built-in class that has no class name.
        TyData::Class(Decl::Class(decl)) => {
            let class = session.types.decl(decl).name.clone();
            if &*class == events::CLASS {
                return Ok("event".to_owned());
            }
            class_name(&class, tree, session, env)
        }
        TyData::UnpackedStruct(Decl::Aggregate { decl, members }) => {
            unpacked_struct_name(decl, &members, tree, session, env)
        }
        // 20.6.1 has no rule for a virtual interface; it is written as it is
        // declared (25.9).
        TyData::VirtualInterface {
            interface: Decl::Interface(interface),
            modport,
        } => Ok(
            match (
                virtual_interfaces::of_type(&session.types, id).map(|ty| ty.spelling()),
                modport,
            ) {
                (Some(spelling), _) => spelling,
                (None, Some(modport)) => {
                    format!("virtual {}.{modport}", session.types.decl(interface).name)
                }
                (None, None) => format!("virtual {}", session.types.decl(interface).name),
            },
        ),
        TyData::Null
        | TyData::Class(_)
        | TyData::VirtualInterface { .. }
        | TyData::UnpackedStruct(_)
        | TyData::Array { .. } => Err(unsupported_operand("this class type")),
    }
}

/// A packed type: a vector, an enumeration, a packed structure or union, or
/// a packed array of one.
fn packed_name(
    packed: &semantic_types::Packed,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    match &packed.element {
        PackedElement::Enum { decl, array } => {
            if *array {
                return Err(unsupported_operand("a packed array of an enumerated type"));
            }
            enum_name(packed, decl, tree, session, env)
        }
        PackedElement::Aggregate(Decl::Aggregate { decl, members }) => {
            aggregate_name(packed, *decl, members, tree, session, env)
        }
        PackedElement::Aggregate(_) => Err(unsupported_operand("this packed type")),
        // No declaration without a typedef has this type (7.4.1).
        PackedElement::Bits if packed.signed_element_depth.is_some() => {
            Err(unsupported_operand("a packed array of a signed type"))
        }
        PackedElement::Bits => Ok(vector_name(packed)),
    }
}

/// A simple bit vector type, or an integer type with a predefined width
/// (6.11), which `$typename` tells apart from a vector of its width.
fn vector_name(packed: &semantic_types::Packed) -> String {
    let (mut name, signed) = match packed.keyword {
        Some(keyword) => (keyword.to_owned(), keyword != "time"),
        // `reg` is `logic` (6.11.2).
        None if packed.four_state => ("logic".to_owned(), false),
        None => ("bit".to_owned(), false),
    };
    // Only a signing other than the default is written (rule b).
    if packed.signed != signed {
        name.push_str(if packed.signed {
            " signed"
        } else {
            " unsigned"
        });
    }
    if packed.keyword.is_none() {
        for range in &packed.dims {
            name.push_str(&format!("[{}:{}]", range.left, range.right));
        }
    }
    name
}

/// An enumeration, with its named constants and their values (rule e).
fn enum_name(
    packed: &semantic_types::Packed,
    decl: &Decl,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    let (domain, named) = match decl {
        Decl::Named(decl) => (session.types.decl(*decl).domain.clone(), Some(*decl)),
        Decl::AnonymousEnum { domain, .. } => (domain.clone(), None),
        _ => (None, None),
    };
    let domain = domain
        .ok_or_else(|| unsupported_operand("an enumerated type with values that are not known"))?;
    let base = vector_name(&semantic_types::Packed {
        element: PackedElement::Bits,
        ..packed.clone()
    });
    let mut name = if base == "int" {
        "enum".to_owned()
    } else {
        format!("enum {base}")
    };
    name.push('{');
    let signing = if domain.signed { "s" } else { "" };
    let width = domain.width;
    let constants = domain
        .members
        .iter()
        .map(|member| {
            let (sign, magnitude) = match domain.value_of(member) {
                crate::ast::enums::MemberValue::Negative(value) => ("-", value.unsigned_abs()),
                crate::ast::enums::MemberValue::NonNegative(value) => ("", value),
            };
            format!("{}={sign}{width}'{signing}d{magnitude}", member.name)
        })
        .collect::<Vec<_>>();
    name.push_str(&constants.join(","));
    name.push('}');
    name.push_str(&typedef_name(
        named,
        "an anonymous enumerated type",
        tree,
        session,
        env,
    )?);
    Ok(name)
}

/// A packed struct or union, with its members.
fn aggregate_name(
    packed: &semantic_types::Packed,
    decl: DeclId,
    members: &[(Arc<str>, TyId)],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    let aggregate = session
        .types
        .decl(decl)
        .aggregate
        .ok_or_else(|| unsupported_operand("a soft or tagged union"))?;
    // The last packed range is the aggregate itself; those before it are
    // dimensions of a packed array of it.
    let outer = &packed.dims[..packed.dims.len().saturating_sub(1)];
    let signed = match (outer.len(), packed.signed_element_depth) {
        (0, _) => packed.signed,
        (_, None) => false,
        (depth, Some(signed)) if signed == depth => true,
        _ => return Err(unsupported_operand("a packed array of a signed type")),
    };
    let mut name = format!("{aggregate} packed");
    if signed {
        name.push_str(" signed");
    }
    name.push('{');
    for (member, r#type) in members {
        name.push_str(&name_of(*r#type, tree, session, env)?);
        name.push(' ');
        name.push_str(member);
        name.push(';');
    }
    name.push('}');
    name.push_str(&typedef_name(
        Some(decl),
        "an anonymous struct or union type",
        tree,
        session,
        env,
    )?);
    for range in outer {
        name.push_str(&format!("[{}:{}]", range.left, range.right));
    }
    Ok(name)
}

/// An unpacked struct with its members, as the example of 20.6.1 writes
/// `struct{bit A;bit B;}top.AB_t`.
fn unpacked_struct_name(
    decl: DeclId,
    members: &[(Arc<str>, TyId)],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    let mut name = "struct{".to_owned();
    for (member, r#type) in members {
        // 20.6.1 has no spelling of the name of a member array.
        if matches!(session.types.data(*r#type), TyData::Array { .. }) {
            return Err(unsupported_operand(
                "an unpacked structure with an unpacked array member",
            ));
        }
        name.push_str(&name_of(*r#type, tree, session, env)?);
        name.push(' ');
        name.push_str(member);
        name.push(';');
    }
    name.push('}');
    name.push_str(&typedef_name(
        Some(decl),
        "an anonymous struct or union type",
        tree,
        session,
        env,
    )?);
    Ok(name)
}

/// The scope-qualified name of the typedef that declares `decl` (rule f).
/// The system-generated name of an anonymous type (rule c) is not
/// supported.
fn typedef_name(
    decl: Option<DeclId>,
    anonymous: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    match decl.and_then(|decl| session.types.decl(decl).typename.clone()) {
        Some(TypedefName::Scope(name)) => Ok(name.to_string()),
        Some(TypedefName::Class { class, name }) => Ok(format!(
            "{}::{name}",
            class_name(&class, tree, session, env)?
        )),
        None => Err(unsupported_operand(anonymous)),
    }
}

/// The name of a method of class specialization `class` in the hierarchical
/// name `%m` prints (IEEE 1800-2023 21.2.1.5): the class as `$typename`
/// names it, or, when a parameter value of the specialization has no such
/// name, the class as its declaration names it.
pub(super) fn class_scope_name(
    class: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Option<String> {
    class_name(class, tree, session, env)
        .ok()
        .or_else(|| declared_class_name(class, tree, session).ok())
}

/// The name of the class that `class`, a class or a specialization of
/// one, specializes: prefixed with the scope that declares it, without
/// parameter values.
fn declared_class_name(
    class: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
) -> Converted<String> {
    let generic = classes::specializations::lookup(session, class)
        .or_else(|| session.foreign.specialization(class))
        .map_or_else(
            || class.to_owned(),
            |specialization| specialization.generic.clone(),
        );
    match classes::declaration(tree, session, &generic) {
        Some(declaration) => Ok(scoped_name(
            classes::enclosing_unit(tree.declarations(), declaration.tokens),
            &instances::normalize_identifier(&declaration.name.spelling),
        )),
        // A class of a package that another scope declares is named by
        // its package already; one of a compilation unit of another file,
        // by the identifier it declares.
        None if generic.contains("::") && !generic.starts_with(packages::UNIT) => Ok(generic),
        None if let Some(identifier) = classes::unit_class_identifier(&generic) => {
            Ok(scoped_name(None, identifier))
        }
        None => Err(unsupported_operand(format!("class `{class}`"))),
    }
}

/// Class specialization `class`, prefixed with the scope that declares the
/// class, followed by the parameter values of a parameterized class (8.25).
fn class_name(
    class: &str,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<String> {
    if class == processes::CLASS {
        return Ok("std::process".to_owned());
    }
    let specialization = classes::specializations::lookup(session, class)
        .or_else(|| session.foreign.specialization(class));
    let generic = specialization.as_ref().map_or_else(
        || class.to_owned(),
        |specialization| specialization.generic.clone(),
    );
    let declaration = classes::declaration(tree, session, &generic);
    let mut name = declared_class_name(class, tree, session)?;
    let parameterized = match declaration {
        Some(declaration) => declaration.parameter_ports.is_some(),
        None => class != generic,
    };
    if parameterized {
        let arguments = class_arguments(class, specialization, tree, session, env)?;
        let arguments = arguments
            .iter()
            .map(|argument| match argument {
                semantic_types::ClassArgument::Type(id) => name_of(*id, tree, session, env),
                semantic_types::ClassArgument::Value(Some(value)) => Ok(value.to_string()),
                semantic_types::ClassArgument::Value(None) => Err(unsupported_operand(format!(
                    "class `{name}` with a parameter value that is not integral"
                ))),
            })
            .collect::<Converted<Vec<_>>>()?;
        name.push_str(&format!("#({})", arguments.join(",")));
    }
    Ok(name)
}

/// The parameter values of class specialization `class`, which its
/// declaration records the first time they are asked for: the types
/// [`specializations::Specialization::arguments`] gives, in the constants
/// of `env` where they are not bound.
fn class_arguments(
    class: &str,
    specialization: Option<std::rc::Rc<classes::specializations::Specialization>>,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Converted<Arc<[semantic_types::ClassArgument]>> {
    let Decl::Class(decl) = session.types.class(class) else {
        unreachable!("a class declares a class");
    };
    if let Some(arguments) = &session.types.decl(decl).arguments {
        return Ok(arguments.clone());
    }
    let specialization = specialization
        .ok_or_else(|| unsupported_operand(format!("class `{class}` without its parameters")))?;
    let arguments = specialization
        .arguments
        .iter()
        .map(|argument| match argument {
            ClassArgument::Type(r#type) => {
                semantic_types::of_declared(&mut session.types, &tree.functions, r#type, env)
                    .map(semantic_types::ClassArgument::Type)
                    .ok_or_else(|| unsupported_operand("a type with a range that is not constant"))
            }
            ClassArgument::Value(value) => Ok(semantic_types::ClassArgument::Value(*value)),
        })
        .collect::<Converted<Arc<[_]>>>()?;
    session
        .types
        .record_class_arguments(decl, arguments.clone());
    Ok(arguments)
}
