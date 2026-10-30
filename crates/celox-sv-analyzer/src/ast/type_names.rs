//! `$typename` (IEEE 1800-2023 20.6.1): the string that represents the
//! resolved type of a data type or of an expression.
//!
//! The string is built from the bound [`Type`]: typedefs are already
//! resolved, a signing equal to the default is not written, ranges are
//! written as `[left:right]` decimal numbers, an unpacked dimension follows
//! the `$` that stands for the name of the array, and an enumeration,
//! struct or union is written with its members and the scope-qualified
//! name of its typedef. A type this cannot spell exactly, such as an
//! anonymous enumeration, is rejected rather than spelled differently.
use super::*;
use system_functions::ContainerKind;

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
    /// whose `$typename` name is `name`.
    pub(super) fn name(&mut self, declaration: Arc<str>, name: TypedefName) {
        let mut parts = self.0.as_deref().cloned().unwrap_or_default();
        parts.declaration = Some(declaration);
        parts.name = Some(name);
        self.0 = Some(Arc::new(parts));
    }

    /// The identity of the typedef that names the type.
    pub(super) fn declaration(&self) -> Option<&str> {
        self.parts()?.declaration.as_deref()
    }

    fn parts(&self) -> Option<&SpellingParts> {
        self.0.as_deref()
    }

    fn keyword_of(&self) -> Option<&'static str> {
        self.parts()?.keyword
    }

    fn aggregate_of(&self) -> Option<&'static str> {
        self.parts()?.aggregate
    }

    fn name_of(&self) -> Option<&TypedefName> {
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
#[derive(Debug, Clone)]
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

/// The declared type of a variable, for `$typename` of its name. Shape
/// equality does not compare it.
#[derive(Debug, Clone, Default)]
pub(super) struct Declared(Option<Arc<Type>>);

impl Declared {
    pub(super) fn new(r#type: &Type) -> Self {
        Self(Some(Arc::new(r#type.clone())))
    }

    /// The declared type, if the shape still has its declared dimensions.
    fn of(shape: &VariableDimensions) -> Option<&Type> {
        let r#type = shape.declared.0.as_deref()?;
        (r#type.packed_ranges.len() == shape.packed.len()
            && r#type.unpacked_ranges.len() == shape.unpacked.len()
            && r#type.is_signed == shape.signed
            && r#type.container.is_some() == shape.container.is_some()
            && r#type.class() == shape.class.as_deref())
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
    context: &PackedDimensions,
) -> Converted<String> {
    let context = dimensions::query_context_for_operand(argument, tree, context)?;
    let r#type = operand_type(argument, tree, &context)?;
    type_name(&r#type, tree, &context.const_env)
}

/// The value of a `$typename(argument)` call: the string as a literal.
pub(super) fn query_value(
    argument: celox_sv_syntax::ExprId,
    tree: &AnalysisContext<'_>,
    context: &PackedDimensions,
) -> Converted<Expr> {
    let name = query_string(argument, tree, context)?;
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
                    selected_base(argument, native),
                    context,
                )?,
            }
        {
            let ranges = type_binding::binding::packed_ranges(&dimensions, tree, context)?;
            return Ok(types::enclose_aliased_packed_type(base, ranges));
        }
    }
    match &native.expression(argument).kind {
        celox_sv_syntax::ExprKind::Type(id) => type_binding::bind(*id, tree, context)?
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
                expression_binding::object_property(tree, argument, native, context)?
            {
                return Ok(property.r#type.clone());
            }
            if let Some(r#type) = context.type_aliases.get(&name) {
                return Ok(r#type.clone());
            }
            if let Some(r#type) = class_operand(tree, &name, context)? {
                return Ok(r#type);
            }
            Err(unsupported_operand(format!("`{name}`")))
        }
        celox_sv_syntax::ExprKind::Member { .. } => {
            if let Some((property, _)) =
                expression_binding::object_property(tree, argument, native, context)?
            {
                return Ok(property.r#type.clone());
            }
            // A static property, `C::v` or `h.v` (8.9).
            if let Some(property) = expression_binding::object_static_property_declaration(
                tree, argument, native, context,
            )? {
                return Ok(property.r#type);
            }
            // A type of a package or class, `p::t` or `C::T`.
            if let Some(r#type) = classes::specializations::scoped_type(tree, argument, context)? {
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
                && let Some(r#type) = class_operand(
                    tree,
                    &scope::qualified_name(
                        &tree.state,
                        &instances::normalize_identifier(&package.spelling),
                        &instances::normalize_identifier(&name.spelling),
                    ),
                    context,
                )?
            {
                return Ok(r#type);
            }
            Err(unsupported_operand("this member or hierarchical name"))
        }
        celox_sv_syntax::ExprKind::Parameterized { .. } => {
            expression_binding::named_class(tree, argument, native, context)?
                .map(|class| classes::handle(&class))
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
    name: &str,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    let Some(class) = classes::resolve(tree, name) else {
        return Ok(None);
    };
    // A parameterized class named alone is its default specialization
    // (8.25).
    if classes::is_parameterized(tree, &class) {
        let class = classes::specializations::specialize(tree, &class, &[], context)?;
        return Ok(Some(classes::handle(&class)));
    }
    Ok(classes::handle_type(tree, &class))
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
    env: &HashMap<String, i128>,
) -> Converted<String> {
    let range = |left: &ConstExpr, right: &ConstExpr| -> Converted<String> {
        let bound = |bound: &ConstExpr| {
            eval_ast_const_expr(&tree.state, bound, env)
                .ok_or_else(|| unsupported_operand("a type with a range that is not constant"))
        };
        Ok(format!("[{}:{}]", bound(left)?, bound(right)?))
    };
    let mut dimensions = String::new();
    for unpacked in &r#type.unpacked_ranges {
        dimensions.push_str(&range(&unpacked.left, &unpacked.right)?);
    }
    let element = match &r#type.container {
        Some(container) => {
            dimensions.push_str(&match container.layout.kind {
                ContainerKind::Dynamic => "[]".to_owned(),
                ContainerKind::Queue { capacity: 0 } => "[$]".to_owned(),
                ContainerKind::Queue { capacity } => format!("[$:{}]", capacity - 1),
                ContainerKind::Associative { .. } => match &container.key {
                    Some(key) => format!("[{}]", type_name(key, tree, env)?),
                    None => "[*]".to_owned(),
                },
            });
            &container.element
        }
        None => r#type,
    };
    if element.container.is_some()
        || (!std::ptr::eq(element, r#type) && !element.unpacked_ranges.is_empty())
    {
        return Err(unsupported_operand("an array of containers"));
    }
    let mut name = element_name(element, tree, env, &range)?;
    if !dimensions.is_empty() {
        // `$` stands for the name of the unpacked array (rule d).
        name.push('$');
        name.push_str(&dimensions);
    }
    Ok(name)
}

/// The name of the element of an unpacked array of type `r#type`.
fn element_name(
    r#type: &Type,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    range: &dyn Fn(&ConstExpr, &ConstExpr) -> Converted<String>,
) -> Converted<String> {
    // An event is a handle of a built-in class that has no class name.
    if let Some(class) = r#type.class().filter(|_| r#type.kind != TypeKind::Event) {
        return class_name(class, tree, env);
    }
    match r#type.kind {
        TypeKind::Event => return Ok("event".to_owned()),
        TypeKind::Real => return Ok("real".to_owned()),
        TypeKind::Shortreal => return Ok("shortreal".to_owned()),
        TypeKind::String => return Ok("string".to_owned()),
        TypeKind::Object | TypeKind::Process => {
            return Err(unsupported_operand("this class type"));
        }
        TypeKind::Bit | TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit => {}
    }
    if let Some(aggregate) = r#type.unpacked_struct() {
        return unpacked_struct_name(r#type, aggregate, tree, env);
    }
    if r#type.is_enum {
        return enum_name(r#type, tree, env, range);
    }
    if !r#type.members.is_empty() {
        return aggregate_name(r#type, tree, env, range);
    }
    if r#type.signed_element_depth.is_some() {
        // No declaration without a typedef has this type (7.4.1).
        return Err(unsupported_operand("a packed array of a signed type"));
    }
    vector_name(r#type, tree, env, range)
}

/// A simple bit vector type, or an integer type with a predefined width
/// (6.11), which `$typename` tells apart from a vector of its width.
fn vector_name(
    r#type: &Type,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    range: &dyn Fn(&ConstExpr, &ConstExpr) -> Converted<String>,
) -> Converted<String> {
    let keyword = r#type.spelling.keyword_of().filter(|keyword| {
        let (kind, width) = match *keyword {
            "byte" => (TypeKind::Bit, 8),
            "shortint" => (TypeKind::Bit, 16),
            "int" => (TypeKind::Bit, 32),
            "longint" => (TypeKind::Bit, 64),
            "integer" => (TypeKind::Logic, 32),
            "time" => (TypeKind::Logic, 64),
            _ => return false,
        };
        r#type.kind == kind
            && matches!(r#type.packed_ranges.as_slice(), [only]
                if eval_ast_const_expr(&tree.state, &only.left, env) == Some(width - 1)
                    && eval_ast_const_expr(&tree.state, &only.right, env) == Some(0))
    });
    let (mut name, signed) = match keyword {
        Some(keyword) => (keyword.to_owned(), keyword != "time"),
        // `reg` is `logic` (6.11.2).
        None if r#type.kind == TypeKind::Bit => ("bit".to_owned(), false),
        None => ("logic".to_owned(), false),
    };
    // Only a signing other than the default is written (rule b).
    if r#type.is_signed != signed {
        name.push_str(if r#type.is_signed {
            " signed"
        } else {
            " unsigned"
        });
    }
    if keyword.is_none() {
        for packed in &r#type.packed_ranges {
            name.push_str(&range(&packed.left, &packed.right)?);
        }
    }
    Ok(name)
}

/// An enumeration, with its named constants and their values (rule e).
fn enum_name(
    r#type: &Type,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    range: &dyn Fn(&ConstExpr, &ConstExpr) -> Converted<String>,
) -> Converted<String> {
    if r#type.packed_enum {
        return Err(unsupported_operand("a packed array of an enumerated type"));
    }
    let domain = r#type
        .enum_domain
        .as_deref()
        .ok_or_else(|| unsupported_operand("an enumerated type with values that are not known"))?;
    let mut base = r#type.clone();
    base.is_enum = false;
    base.enum_domain = None;
    let base = vector_name(&base, tree, env, range)?;
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
            let sign = if member.value < 0 { "-" } else { "" };
            format!(
                "{}={sign}{width}'{signing}d{}",
                member.name,
                member.value.unsigned_abs()
            )
        })
        .collect::<Vec<_>>();
    name.push_str(&constants.join(","));
    name.push('}');
    name.push_str(&typedef_name(
        r#type,
        "an anonymous enumerated type",
        tree,
        env,
    )?);
    Ok(name)
}

/// A packed struct or union, with its members.
fn aggregate_name(
    r#type: &Type,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    range: &dyn Fn(&ConstExpr, &ConstExpr) -> Converted<String>,
) -> Converted<String> {
    let aggregate = r#type
        .spelling
        .aggregate_of()
        .ok_or_else(|| unsupported_operand("a soft or tagged union"))?;
    // The last packed range is the aggregate itself; those before it are
    // dimensions of a packed array of it.
    let outer = &r#type.packed_ranges[..r#type.packed_ranges.len().saturating_sub(1)];
    let signed = match (outer.len(), r#type.signed_element_depth) {
        (0, _) => r#type.is_signed,
        (_, None) => false,
        (depth, Some(signed)) if signed == depth => true,
        _ => return Err(unsupported_operand("a packed array of a signed type")),
    };
    let mut name = format!("{aggregate} packed");
    if signed {
        name.push_str(" signed");
    }
    name.push('{');
    for member in &r#type.members {
        name.push_str(&type_name(member.r#type(), tree, env)?);
        name.push(' ');
        name.push_str(member.name());
        name.push(';');
    }
    name.push('}');
    name.push_str(&typedef_name(
        r#type,
        "an anonymous struct or union type",
        tree,
        env,
    )?);
    for packed in outer {
        name.push_str(&range(&packed.left, &packed.right)?);
    }
    Ok(name)
}

/// An unpacked struct with its members, as the example of 20.6.1 writes
/// `struct{bit A;bit B;}top.AB_t`.
fn unpacked_struct_name(
    r#type: &Type,
    aggregate: &unpacked_structs::UnpackedStruct,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
) -> Converted<String> {
    let mut name = "struct{".to_owned();
    for member in aggregate.members() {
        // 20.6.1 has no spelling of the name of a member array.
        if !member.r#type.unpacked_ranges.is_empty() || member.r#type.container.is_some() {
            return Err(unsupported_operand(
                "an unpacked structure with an unpacked array member",
            ));
        }
        name.push_str(&type_name(&member.r#type, tree, env)?);
        name.push(' ');
        name.push_str(&member.name);
        name.push(';');
    }
    name.push('}');
    name.push_str(&typedef_name(
        r#type,
        "an anonymous struct or union type",
        tree,
        env,
    )?);
    Ok(name)
}

/// The scope-qualified name of the typedef of `r#type` (rule f). The
/// system-generated name of an anonymous type (rule c) is not supported.
fn typedef_name(
    r#type: &Type,
    anonymous: &str,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
) -> Converted<String> {
    match r#type.spelling.name_of() {
        Some(TypedefName::Scope(name)) => Ok(name.to_string()),
        Some(TypedefName::Class { class, name }) => {
            Ok(format!("{}::{name}", class_name(class, tree, env)?))
        }
        None => Err(unsupported_operand(anonymous)),
    }
}

/// Class specialization `class`, prefixed with the scope that declares the
/// class, followed by the parameter values of a parameterized class (8.25).
fn class_name(
    class: &str,
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
) -> Converted<String> {
    if class == processes::CLASS {
        return Ok("std::process".to_owned());
    }
    let specialization = classes::specializations::lookup(tree, class)
        .or_else(|| tree.state.foreign.specialization(class));
    let generic = specialization.as_ref().map_or_else(
        || class.to_owned(),
        |specialization| specialization.generic.clone(),
    );
    let declaration = classes::declaration(tree, &generic);
    let mut name = match declaration {
        Some(declaration) => scoped_name(
            classes::enclosing_unit(tree.declarations(), declaration.tokens),
            &instances::normalize_identifier(&declaration.name.spelling),
        ),
        // A class of a package that another scope declares is named by
        // its package already.
        None if generic.contains("::") && !generic.starts_with(packages::UNIT) => generic.clone(),
        None => return Err(unsupported_operand(format!("class `{class}`"))),
    };
    let parameterized = match declaration {
        Some(declaration) => declaration.parameter_ports.is_some(),
        None => class != generic,
    };
    if parameterized {
        let specialization = specialization.ok_or_else(|| {
            unsupported_operand(format!("class `{class}` without its parameters"))
        })?;
        let arguments = specialization
            .arguments
            .iter()
            .map(|argument| match argument {
                ClassArgument::Type(r#type) => type_name(r#type, tree, env),
                ClassArgument::Value(Some(value)) => Ok(value.to_string()),
                ClassArgument::Value(None) => Err(unsupported_operand(format!(
                    "class `{name}` with a parameter value that is not integral"
                ))),
            })
            .collect::<Converted<Vec<_>>>()?;
        name.push_str(&format!("#({})", arguments.join(",")));
    }
    Ok(name)
}
