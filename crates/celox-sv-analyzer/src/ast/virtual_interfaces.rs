//! Virtual interfaces (IEEE 1800-2023 25.9).
//!
//! Interfaces are flattened into the signals of the scopes that instantiate
//! them, so there is no run-time object for a virtual interface to refer to.
//! A virtual interface value is instead a number: 0 for `null`, and for each
//! interface instance of the elaborated design one id, which the frontend
//! assigns (see [`VirtualInterfaces`]). A member access through a virtual
//! interface selects among the instances its type admits, each of whose
//! members the module refers to through a signal bound to that instance's
//! member. Every access is thus a static access of a signal, and designs
//! without virtual interfaces are unaffected.

use super::*;

/// Bit width of a virtual interface value.
pub(super) const WIDTH: usize = 32;

/// The prefix of the signals that denote a member of an interface instance
/// for virtual interface accesses: `$vif$<id>$<member>`.
const MEMBER_PREFIX: &str = "$vif$";

/// The prefix of the signal that holds the id of an interface instance or
/// port of a module: `$vifid$<handle>`.
const IDENTITY_PREFIX: &str = "$vifid$";
/// The prefix of the signal a select through a virtual interface member is
/// bound against before it is applied to the member of each instance.
const TEMPLATE_PREFIX: &str = "$viftemplate$";
/// The prefix of the signal that stands for member `<member>` of whichever
/// instance of interface `<interface>` an access selects, before the
/// frontend knows the instances: `$vifany$<interface>$<member>`.
const PLACEHOLDER_PREFIX: &str = "$vifany$";
/// The prefix of the signal that stands for the id of that instance:
/// `$vifinstance$<interface>`.
const INSTANCE_PREFIX: &str = "$vifinstance$";

/// A virtual interface type: an interface, with a modport if one is
/// selected (IEEE 1800-2023 25.9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VirtualInterface {
    pub interface: String,
    pub modport: Option<String>,
}

impl VirtualInterface {
    /// The semantic type of a virtual interface of this type.
    pub(super) fn type_id(&self, table: &mut semantic_types::TyTable) -> semantic_types::TyId {
        table.intern(semantic_types::TyData::VirtualInterface {
            interface: semantic_types::Decl::Interface(self.interface.as_str().into()),
            modport: self.modport.as_deref().map(Into::into),
        })
    }

    /// How the type is spelled in source.
    pub(super) fn spelling(&self) -> String {
        match &self.modport {
            Some(modport) => format!("virtual {}.{modport}", self.interface),
            None => format!("virtual {}", self.interface),
        }
    }
}

/// The interfaces that virtual interface types of a design name, from the
/// interface signatures. Analysis does not know their instances, which the
/// frontend numbers in the elaborated hierarchy and repeats each access for
/// (see [`crate::ir::Module::expand_virtual_interface_instances`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualInterfaces {
    pub interfaces: HashMap<String, VirtualInterfaceDeclaration>,
}

/// An interface that a virtual interface type names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualInterfaceDeclaration {
    /// The ports and the variables and nets the interface declares.
    pub members: Vec<VirtualInterfaceMember>,
    /// Each modport with the members it lists and their directions.
    pub modports: HashMap<String, Vec<(String, String)>>,
    /// The interface has parameters, whose values are part of the type.
    pub parameterized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualInterfaceMember {
    pub name: String,
    pub r#type: Type,
    pub is_net: bool,
}

impl VirtualInterfaceDeclaration {
    fn member(&self, name: &str) -> Option<&VirtualInterfaceMember> {
        self.members.iter().find(|member| member.name == name)
    }
}

impl VirtualInterfaces {
    /// The declarations of the interfaces that the virtual interface types of
    /// `parsed` name, from the interface signatures of `sources`, with no
    /// instances yet; `None` if there are none.
    pub fn declarations_of(
        sources: &[(&str, &std::path::Path)],
        parsed: &[&crate::ParsedSource],
        options: &crate::AnalysisOptions,
    ) -> Result<Option<Arc<VirtualInterfaces>>, AnalyzerError> {
        let names = parsed
            .iter()
            .flat_map(|source| source.virtual_interface_names())
            .collect::<HashSet<_>>();
        if names.is_empty() {
            return Ok(None);
        }
        let mut declarations = VirtualInterfaces::default();
        // Building the prototypes needs the packages only when an interface
        // names one of their declarations.
        let prototypes =
            interfaces::prototypes_without_packages(sources, Some(&names), options.clone())
                .or_else(|_| {
                    interfaces::interface_prototypes_with_options(
                        sources,
                        Some(&names),
                        options.clone(),
                    )
                })?;
        for prototype in prototypes {
            let module = &prototype.module;
            // Member bounds may name the interface's constants.
            let constants = module
                .parameters()
                .iter()
                .filter_map(|parameter| {
                    Some((parameter.name().to_owned(), parameter.resolved_value()?))
                })
                .collect::<ConstEnv>();
            // The signature is a module with a port of the interface, whose
            // members are its ports `<port>$<member>`.
            let members = module
                .ports()
                .iter()
                .filter_map(|port| {
                    let (_, name) = port.name().split_once('$')?;
                    Some((name, port.r#type(), port.is_net()))
                })
                .filter_map(|(name, r#type, is_net)| {
                    Some(VirtualInterfaceMember {
                        name: name.to_string(),
                        r#type: resolved_type(r#type, &constants)?,
                        is_net,
                    })
                })
                .collect();
            let modports = prototype
                .modports
                .iter()
                .map(|(name, items)| {
                    (
                        name.clone(),
                        items
                            .iter()
                            .filter(|(_, direction)| direction != "import")
                            .cloned()
                            .collect(),
                    )
                })
                .collect();
            declarations.interfaces.insert(
                prototype.name.clone(),
                VirtualInterfaceDeclaration {
                    members,
                    modports,
                    parameterized: !prototype.parameters.is_empty(),
                },
            );
        }
        Ok(Some(Arc::new(declarations)))
    }
}

/// `r#type` of a member of a non-parameterized interface, whose bounds are
/// constants, possibly the interface's `constants`.
fn resolved_type(r#type: &crate::ir::Type, constants: &ConstEnv) -> Option<Type> {
    let kind = match r#type.kind() {
        crate::ir::TypeKind::Bit => TypeKind::Bit,
        crate::ir::TypeKind::Logic => TypeKind::Logic,
        crate::ir::TypeKind::Reg => TypeKind::Reg,
        crate::ir::TypeKind::Implicit => TypeKind::Implicit,
        _ => return None,
    };
    let types = Default::default();
    let eval = |expr| typecheck::eval_const_expr_with_types(expr, constants, &types);
    let packed = r#type
        .packed_ranges()
        .iter()
        .map(|range| Some((eval(range.left())?, eval(range.right())?)))
        .collect::<Option<Vec<_>>>()?;
    if !r#type.unpacked_ranges().is_empty() {
        return None;
    }
    Some(
        Type::resolved(
            kind,
            r#type.is_signed(),
            r#type.signed_element_depth(),
            &packed,
            &[],
        )
        .with_chandle(r#type.is_chandle()),
    )
}

/// A signal of a module that the frontend binds for virtual interfaces.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VirtualInterfaceSignal {
    /// Member `member` of the interface instance `id`.
    Member { id: u32, member: String },
    /// The id of the interface instance that the module's interface
    /// instance or port `handle` denotes, in each instance of the module.
    Identity { handle: String },
    /// Member `member` of interface `interface`, which a select through a
    /// virtual interface is bound against before it is applied to the member
    /// of each instance; it denotes no instance's member.
    Template { interface: String, member: String },
    /// Member `member` of the instance of interface `interface` that an
    /// access selects: analysis builds each access once with it, for the
    /// frontend to repeat for every instance of the elaborated design (see
    /// [`crate::ir::Module::expand_virtual_interface_instances`]).
    Placeholder { interface: String, member: String },
    /// The id of that instance, which the access compares its handle with.
    Instance { interface: String },
}

impl VirtualInterfaceSignal {
    /// The kind of a signal named `name`, if the frontend binds it.
    pub fn of(name: &str) -> Option<Self> {
        if let Some((interface, member)) = name
            .strip_prefix(PLACEHOLDER_PREFIX)
            .and_then(|rest| rest.split_once('$'))
        {
            return Some(Self::Placeholder {
                interface: interface.to_owned(),
                member: member.to_owned(),
            });
        }
        if let Some(interface) = name.strip_prefix(INSTANCE_PREFIX) {
            return Some(Self::Instance {
                interface: interface.to_owned(),
            });
        }
        if let Some(handle) = name.strip_prefix(IDENTITY_PREFIX) {
            return Some(Self::Identity {
                handle: handle.to_owned(),
            });
        }
        if let Some((interface, member)) = name
            .strip_prefix(TEMPLATE_PREFIX)
            .and_then(|rest| rest.split_once('$'))
        {
            return Some(Self::Template {
                interface: interface.to_owned(),
                member: member.to_owned(),
            });
        }
        let (id, member) = name.strip_prefix(MEMBER_PREFIX)?.split_once('$')?;
        Some(Self::Member {
            id: id.parse().ok()?,
            member: member.to_owned(),
        })
    }

    pub(crate) fn name(&self) -> String {
        match self {
            Self::Member { id, member } => format!("{MEMBER_PREFIX}{id}${member}"),
            Self::Identity { handle } => format!("{IDENTITY_PREFIX}{handle}"),
            Self::Template { interface, member } => {
                format!("{TEMPLATE_PREFIX}{interface}${member}")
            }
            Self::Placeholder { interface, member } => {
                format!("{PLACEHOLDER_PREFIX}{interface}${member}")
            }
            Self::Instance { interface } => format!("{INSTANCE_PREFIX}{interface}"),
        }
    }
}

/// The type of a virtual interface value: an unsigned two-state number.
pub(super) fn value_type(interface: VirtualInterface) -> Type {
    let mut ty = Type::new(TypeKind::Bit);
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal((WIDTH - 1).to_string()),
        ConstExpr::Literal("0".into()),
    ));
    ty.virtual_interface = Some(Arc::new(interface));
    ty
}

/// The declarations of the design's virtual interfaces, if it has any.
pub(super) fn declarations(state: &Session) -> Option<&VirtualInterfaces> {
    state.virtual_interfaces.as_deref()
}

/// The type `virtual name[.modport]`, checked against the interface.
pub(super) fn bind_type(
    state: &Session,
    name: &celox_sv_syntax::Name,
    arguments: Option<&[celox_sv_syntax::Argument]>,
    modport: Option<&celox_sv_syntax::Name>,
) -> Converted<Type> {
    let interface = instances::normalize_identifier(&name.spelling);
    let modport = modport.map(|modport| instances::normalize_identifier(&modport.spelling));
    // The packages are analyzed once to find the interfaces before the
    // declarations exist; the analysis that uses the types checks them.
    let Some(declarations) = declarations(state) else {
        return Ok(value_type(VirtualInterface { interface, modport }));
    };
    // The declarations hold every interface that a virtual interface type
    // of any source names (see `VirtualInterfaces::declarations_of`), so one
    // they lack is declared in no source (IEEE 1800-2023 25.9).
    let declaration = declarations.interfaces.get(&interface).ok_or_else(|| {
        AnalyzerError::UndeclaredIdentifier {
            name: name.value().to_owned(),
            context: "no interface of this name is declared".into(),
        }
    })?;
    if arguments.is_some() || declaration.parameterized {
        return Err(unsupported(format!(
            "virtual interface type of parameterized interface `{interface}`"
        )));
    }
    if let Some(modport) = &modport
        && !declaration.modports.contains_key(modport)
    {
        return Err(unsupported(format!(
            "modport `{modport}` of interface `{interface}`"
        )));
    }
    Ok(value_type(VirtualInterface { interface, modport }))
}

/// The member `member` that a virtual interface of type `ty` reaches, with
/// the signal that denotes it in the instance the value selects and the id
/// that instance has: placeholders, which the frontend repeats for each
/// instance of the elaborated design, so analysis does not depend on them.
pub(super) fn member<'a>(
    state: &'a Session,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<(&'a VirtualInterfaceMember, Vec<(Expr, String)>)> {
    let declaration = declarations(state)
        .and_then(|declarations| declarations.interfaces.get(&ty.interface))
        .ok_or_else(|| unsupported(format!("virtual interface type of `{}`", ty.interface)))?;
    let found = declaration.member(member).ok_or_else(|| {
        unsupported(format!(
            "member `{member}` of virtual interface `{}`",
            ty.interface
        ))
    })?;
    if let Some(modport) = &ty.modport
        && !declaration.modports[modport]
            .iter()
            .any(|(listed, _)| listed == member)
    {
        return Err(unsupported(format!(
            "member `{member}` outside modport `{}.{modport}`",
            ty.interface
        )));
    }
    let instance = VirtualInterfaceSignal::Instance {
        interface: ty.interface.clone(),
    }
    .name();
    let signal = VirtualInterfaceSignal::Placeholder {
        interface: ty.interface.clone(),
        member: member.to_owned(),
    }
    .name();
    Ok((found, vec![(Expr::Ident(instance), signal)]))
}

/// An error if member `member` of a virtual interface of type `ty` is an
/// input of its modport, which may not be assigned (IEEE 1800-2023 25.5,
/// 23.3.3.4).
pub(super) fn check_write(
    state: &Session,
    ty: &VirtualInterface,
    member: &str,
) -> Result<(), AnalyzerError> {
    let Some(modport) = &ty.modport else {
        return Ok(());
    };
    let input = declarations(state)
        .and_then(|declarations| declarations.interfaces.get(&ty.interface))
        .and_then(|declaration| declaration.modports.get(modport))
        .is_some_and(|members| {
            members
                .iter()
                .any(|(listed, direction)| listed == member && direction == "input")
        });
    if input {
        return Err(AnalyzerError::InputModportAssignment {
            member: member.to_owned(),
            target: ty.spelling(),
        });
    }
    Ok(())
}

/// The signals through which each scope reaches the members of the
/// interface instances that virtual interfaces may denote: the templates
/// selects are bound against, and per interface the placeholders of the
/// selected instance's members and id, which the frontend replaces with the
/// signals of each instance (see [`crate::ir::Module::expand_virtual_interface_instances`]).
pub(super) fn member_signals(declarations: &VirtualInterfaces) -> Vec<Signal> {
    let mut interfaces = declarations.interfaces.iter().collect::<Vec<_>>();
    interfaces.sort_by(|left, right| left.0.cmp(right.0));
    let mut signals = Vec::new();
    for (interface, declaration) in &interfaces {
        signals.extend(declaration.members.iter().map(|member| Signal {
            name: template(interface, &member.name),
            r#type: member.r#type.clone(),
            is_net: false,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            is_const: false,
            package_variable: None,
            hierarchical: None,
        }));
    }
    for (interface, declaration) in interfaces {
        let instance = VirtualInterfaceSignal::Instance {
            interface: interface.clone(),
        }
        .name();
        signals.push(Signal {
            hierarchical: Some(instance.clone()),
            name: instance,
            r#type: value_type(VirtualInterface {
                interface: interface.clone(),
                modport: None,
            }),
            is_net: false,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            is_const: false,
            package_variable: None,
        });
        signals.extend(declaration.members.iter().map(|member| {
            let name = VirtualInterfaceSignal::Placeholder {
                interface: interface.clone(),
                member: member.name.clone(),
            }
            .name();
            // It denotes the member of another instance, as a hierarchical
            // name does: that instance's object, not one of this module.
            Signal {
                hierarchical: Some(name.clone()),
                name,
                r#type: member.r#type.clone(),
                is_net: member.is_net,
                unresolved_net: false,
                net_resolution: NetResolution::Wire,
                net_drive: NetDrive::None,
                is_const: false,
                package_variable: None,
            }
        }));
    }
    signals
}

/// The signal a select through member `member` of a virtual interface of
/// interface `interface` is bound against (see [`selected_read`]).
pub(super) fn template(interface: &str, member: &str) -> String {
    VirtualInterfaceSignal::Template {
        interface: interface.to_owned(),
        member: member.to_owned(),
    }
    .name()
}

/// The member access `h.m` that native expression `id` selects into, as
/// `h.m[i]` or `h.m[i][j:k]`, with the access's handle, type and member.
pub(super) fn native_selected_member(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Option<(celox_sv_syntax::ExprId, Arc<VirtualInterface>, String)> {
    let mut base = expression_binding::ungroup(id, native);
    let mut selected = false;
    while let celox_sv_syntax::ExprKind::Select { expression, .. } = &native.expression(base).kind {
        selected = true;
        base = expression_binding::ungroup(*expression, native);
    }
    selected
        .then(|| native_member(tree, session, base, native, dimensions))
        .flatten()
}

/// `selected`, a select bound against the [`template`] of member `member`,
/// read from the instance that virtual interface `handle` of type `ty`
/// denotes; an error for `null`.
pub(super) fn selected_read(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
    selected: Expr,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let (_, signals) = self::member(session, ty, member)?;
    check_pure(&handle)?;
    let width = expr_static_width(tree, &selected, dimensions)
        .ok_or_else(|| unsupported(format!("width of a select of `{member}`")))?;
    let signed = dimensions::expression_signedness(&selected, dimensions, &dimensions.const_env)
        .unwrap_or(false);
    let from = template(&ty.interface, member);
    let mut value = null_access(&handle, width, signed);
    for (id, signal) in signals.into_iter().rev() {
        value = Expr::Mux {
            condition: Box::new(denotes(&handle, &id)),
            then_expr: Box::new(rename(selected.clone(), &from, &signal)),
            else_expr: Box::new(value),
        };
    }
    Ok(value)
}

/// An error if `target` is a [`template`], which a write reaches only
/// through [`dispatch`]: any other assignment form through a virtual
/// interface member is unsupported.
pub(super) fn check_target(target: &LValue) -> Converted<()> {
    match VirtualInterfaceSignal::of(target.name()) {
        Some(VirtualInterfaceSignal::Template { member, .. }) => Err(unsupported(format!(
            "this form of assignment to `{member}` through a virtual interface"
        ))),
        _ => Ok(()),
    }
}

/// `expr` with identifier `from` renamed to `to`.
pub(super) fn rename(expr: Expr, from: &str, to: &str) -> Expr {
    match expr {
        Expr::Ident(name) if name == from => Expr::Ident(to.to_owned()),
        expr => map_children(expr, &mut |child| rename(child, from, to)),
    }
}

/// The virtual interface type of native expression `id`: a variable, a
/// property, or an element of an array or a container of virtual
/// interfaces, as its type tells (25.9).
pub(super) fn native_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Option<Arc<VirtualInterface>> {
    declarations(session)?;
    let ty = expression_types::type_of(tree, session, id, native, dimensions).ok()?;
    of_type(&session.types, ty)
}

/// The virtual interface type `ty` is, if it is one.
pub(super) fn of_type(
    types: &semantic_types::TyTable,
    ty: semantic_types::TyId,
) -> Option<Arc<VirtualInterface>> {
    let semantic_types::TyData::VirtualInterface {
        interface: semantic_types::Decl::Interface(interface),
        modport,
    } = types.data(ty)
    else {
        return None;
    };
    Some(Arc::new(VirtualInterface {
        interface: interface.to_string(),
        modport: modport.as_deref().map(str::to_owned),
    }))
}

/// Whether declared shape `shape` is a virtual interface or a fixed-size
/// array of them (25.9), as its type tells.
pub(super) fn holds_virtual_interface(
    shape: &VariableDimensions,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> bool {
    shape
        .type_id(&mut session.types, &tree.functions, &dims.const_env)
        .is_some_and(|ty| {
            of_type(&session.types, ty).is_some() || element_of_array(&session.types, ty).is_some()
        })
}

/// The virtual interface type of the elements of `ty`, if it is a fixed-size
/// unpacked array, of one or more dimensions, of virtual interfaces.
pub(super) fn element_of_array(
    types: &semantic_types::TyTable,
    ty: semantic_types::TyId,
) -> Option<Arc<VirtualInterface>> {
    let mut element = ty;
    let mut dimensions = 0;
    while let semantic_types::TyData::Array {
        kind: semantic_types::ArrayKind::Fixed(_),
        element: inner,
    } = types.data(element)
    {
        element = *inner;
        dimensions += 1;
    }
    (dimensions > 0).then(|| of_type(types, element)).flatten()
}

/// The member access `h.m` that native expression `id` is, through a
/// virtual interface `h`: the native handle, its type and the member.
pub(super) fn native_member(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Option<(celox_sv_syntax::ExprId, Arc<VirtualInterface>, String)> {
    let celox_sv_syntax::ExprKind::Member {
        expression,
        separator,
        name,
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    else {
        return None;
    };
    if separator != "." {
        return None;
    }
    let ty = native_type(tree, session, *expression, native, dimensions)?;
    Some((
        *expression,
        ty,
        instances::normalize_identifier(&name.spelling),
    ))
}

/// The comparison of a virtual interface value with the id of an instance.
fn denotes(handle: &Expr, id: &Expr) -> Expr {
    Expr::Binary {
        left: Box::new(handle.clone()),
        op: BinaryOp::Eq,
        right: Box::new(id.clone()),
    }
}

/// An access through virtual interface `handle` when it denotes none of
/// the instances its type admits: an error for `null` (IEEE 1800-2023
/// 25.9), yielding a `width`-bit zero.
pub(super) fn null_access(handle: &Expr, width: usize, signed: bool) -> Expr {
    Expr::Call {
        callee: Callee::Checked {
            width,
            signed,
            check: system_functions::AccessCheck::VirtualInterface,
        },
        args: vec![
            Some(Expr::Binary {
                left: Box::new(handle.clone()),
                op: BinaryOp::Ne,
                right: Box::new(Expr::Literal(format!("{WIDTH}'d0"))),
            }),
            Some(Expr::Literal(format!("{width}'d0"))),
        ],
        arg_order: Vec::new(),
    }
}

/// A read of member `member` of the interface instance that virtual
/// interface `handle` of type `ty` denotes, with `select` applied to the
/// member of each instance.
pub(super) fn read(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
    dimensions: &PackedDimensions,
    select: impl Fn(Expr) -> Converted<Expr>,
) -> Converted<Expr> {
    let functions = &tree.functions;
    let (found, signals) = self::member(session, ty, member)?;
    check_pure(&handle)?;
    let mut arms = Vec::new();
    for (id, signal) in signals {
        arms.push((id, select(Expr::Ident(signal))?));
    }
    let (width, signed) = match arms.first() {
        Some((_, value)) => (
            expr_static_width(tree, value, dimensions),
            dimensions::expression_signedness(value, dimensions, &dimensions.const_env)
                .unwrap_or(false),
        ),
        None => {
            let shape = casts::expr_type_from_type(functions, &found.r#type, &ConstEnv::default());
            (shape.map(|shape| shape.width), found.r#type.is_signed())
        }
    };
    let width = width
        .ok_or_else(|| unsupported(format!("width of virtual interface member `{member}`")))?;
    let mut value = null_access(&handle, width, signed);
    for (id, arm) in arms.into_iter().rev() {
        value = Expr::Mux {
            condition: Box::new(denotes(&handle, &id)),
            then_expr: Box::new(arm),
            else_expr: Box::new(value),
        };
    }
    Ok(value)
}

/// An error unless evaluating virtual interface `handle` has no effects: a
/// member access compares it with each instance, evaluating it again.
fn check_pure(handle: &Expr) -> Converted<()> {
    if pure(handle) {
        Ok(())
    } else {
        Err(unsupported(
            "virtual interface handle expression with a call that may have effects",
        ))
    }
}

/// The variable handle expression `expr` is read from, through selects,
/// container element reads and checks; `None` for a handle that no
/// variable holds, as a class property of an object or a call's result.
pub(super) fn base_variable(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Ident(name) => Some(name),
        Expr::Select { expr, .. } => base_variable(expr),
        Expr::Call {
            callee: Callee::Checked { .. },
            args,
            ..
        } => match args.as_slice() {
            [Some(_), Some(value)] => base_variable(value),
            _ => None,
        },
        Expr::Call {
            callee: Callee::Object(op),
            args,
            ..
        } if matches!(
            **op,
            system_functions::ObjectOp::Container {
                op: system_functions::ContainerOp::Get,
                ..
            }
        ) =>
        {
            args.first()
                .and_then(Option::as_ref)
                .and_then(base_variable)
        }
        _ => None,
    }
}

/// Whether expression `expr` reads storage only: it has no call but the
/// read of a container element, whose container and index it reads too.
/// A subroutine call, or a property of an object, is no storage of the
/// design.
pub(super) fn reads_storage_only(expr: &Expr) -> bool {
    let mut storage = true;
    visit_calls(expr, &mut |callee| {
        storage &= matches!(callee, Callee::Checked { .. })
            || matches!(callee, Callee::Object(op) if matches!(
                **op,
                system_functions::ObjectOp::Container {
                    op: system_functions::ContainerOp::Get,
                    ..
                }
            ));
    });
    storage
}

/// Whether evaluating virtual interface `handle` has no effects, so that
/// evaluating it again gives the same handle.
pub(super) fn pure(handle: &Expr) -> bool {
    let mut effect = false;
    visit_calls(handle, &mut |callee| {
        effect |= match callee {
            Callee::String(_) | Callee::Checked { .. } => false,
            Callee::Object(op) => !matches!(
                **op,
                system_functions::ObjectOp::Load(_)
                    | system_functions::ObjectOp::Container {
                        op: system_functions::ContainerOp::Get,
                        ..
                    }
            ),
            _ => true,
        };
    });
    !effect
}

/// `visit(callee)` for each call in `expr`, including its select indices.
fn visit_calls(expr: &Expr, visit: &mut dyn FnMut(&Callee)) {
    match expr {
        Expr::Call { callee, .. } => visit(callee),
        Expr::Select { msb, lsb, .. } => {
            visit_const_calls(msb, visit);
            visit_const_calls(lsb, visit);
        }
        _ => {}
    }
    for child in children(expr) {
        visit_calls(child, visit);
    }
}

/// `visit(callee)` for each call in constant-form expression `expr`.
fn visit_const_calls(expr: &ConstExpr, visit: &mut dyn FnMut(&Callee)) {
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::Function { callee, args, .. } => {
            visit(callee);
            for arg in args {
                visit_const_calls(arg, visit);
            }
        }
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            for operand in [expr, msb, lsb] {
                visit_const_calls(operand, visit);
            }
        }
        ConstExpr::Select { expr, bit } => {
            visit_const_calls(expr, visit);
            visit_const_calls(bit, visit);
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            visit_const_calls(expr, visit)
        }
        ConstExpr::Binary { left, right, .. } => {
            visit_const_calls(left, visit);
            visit_const_calls(right, visit);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            for operand in [condition, then_expr, else_expr] {
                visit_const_calls(operand, visit);
            }
        }
    }
}

/// The statements `arm(signal)` for the instance that virtual interface
/// `handle` of type `ty` denotes, `signal` naming its member `member`; an
/// error for `null`.
pub(super) fn dispatch(
    session: &mut Session,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
    mut arm: impl FnMut(&mut Session, String) -> Converted<Vec<Stmt>>,
) -> Converted<Stmt> {
    let (_, signals) = self::member(session, ty, member)?;
    check_pure(&handle)?;
    let items = signals
        .into_iter()
        .map(|(id, signal)| {
            Ok(crate::procedural::CaseItemBase {
                labels: vec![crate::procedural::CaseLabel::Value(id)],
                body: arm(session, signal)?,
            })
        })
        .collect::<Converted<Vec<_>>>()?;
    Ok(Stmt::Case {
        kind: crate::procedural::CaseKind::Exact,
        default: Some(vec![Stmt::Eval(null_access(&handle, 1, false))]),
        selector: handle,
        items,
    })
}

/// The signal that holds the id of the interface instance that interface
/// instance or port `handle` of interface `interface` denotes, if a virtual
/// interface may denote it.
pub(super) fn identity_signal(state: &Session, handle: &str, interface: &str) -> Option<Signal> {
    declarations(state)?.interfaces.get(interface)?;
    Some(Signal {
        name: VirtualInterfaceSignal::Identity {
            handle: handle.to_owned(),
        }
        .name(),
        r#type: value_type(VirtualInterface {
            interface: interface.to_owned(),
            modport: None,
        }),
        is_net: false,
        unresolved_net: false,
        net_resolution: NetResolution::Wire,
        net_drive: NetDrive::None,
        is_const: false,
        package_variable: None,
        hierarchical: None,
    })
}

/// [`identity_signal`] for a port of interface `interface`, unless the
/// interface declares no members: the frontend finds the instance a port
/// denotes by the connections of its members.
pub(super) fn port_identity_signal(
    state: &Session,
    handle: &str,
    interface: &str,
) -> Option<Signal> {
    if declarations(state)?
        .interfaces
        .get(interface)?
        .members
        .is_empty()
    {
        return None;
    }
    identity_signal(state, handle, interface)
}

/// The interface instance or port `name` of the scope as a virtual
/// interface value: the id of the instance it denotes.
pub(super) fn identity(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    dimensions: &PackedDimensions,
) -> Converted<Option<Expr>> {
    if declarations(session).is_none() || dimensions.get(name).is_some() {
        return Ok(None);
    }
    let signal = VirtualInterfaceSignal::Identity {
        handle: name.to_owned(),
    }
    .name();
    if dimensions.get(&signal).is_some() {
        return Ok(Some(Expr::Ident(signal)));
    }
    // A port of an interface without members has no connection the
    // instance it denotes could be found by (see [`port_identity_signal`]).
    match scope::imported(tree).interface_types.get(name) {
        Some(interface) => Err(unsupported(format!(
            "virtual interface value of port `{name}` of interface `{interface}`, which declares no members"
        ))),
        None => Ok(None),
    }
}

/// The interface instances and ports of a scope with symbols `symbols` and
/// ports `ports` that a virtual interface may denote.
pub(super) fn handles(
    state: &Session,
    symbols: &scope::ScopeSymbols,
    ports: &[Port],
) -> Vec<crate::ir::InterfaceHandle> {
    let Some(declarations) = declarations(state) else {
        return Vec::new();
    };
    let mut handles = symbols
        .interface_types
        .iter()
        .filter_map(|(handle, interface)| {
            let declaration = declarations.interfaces.get(interface)?;
            let members = declaration
                .members
                .iter()
                .filter_map(|member| {
                    let signal = symbols
                        .interface_members
                        .get(&format!("{handle}.{}", member.name))?;
                    Some((member.name.clone(), signal.clone()))
                })
                .collect::<Vec<_>>();
            let port = members
                .iter()
                .any(|(_, signal)| ports.iter().any(|port| port.name() == signal));
            Some(crate::ir::InterfaceHandle {
                handle: handle.clone(),
                interface: interface.clone(),
                port,
                members,
            })
        })
        .collect::<Vec<_>>();
    handles.sort_by(|left, right| left.handle.cmp(&right.handle));
    handles
}

/// The element of an interface instance array, or the interface instance of
/// a generate scope, that native expression `id` names, as a virtual
/// interface value.
pub(super) fn native_identity(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    dimensions: &PackedDimensions,
) -> Option<Expr> {
    declarations(session)?;
    let handle = interface_binding::actual_handle_key(
        id,
        tree,
        session,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )
    .ok()?;
    identity(tree, session, &handle, dimensions).ok().flatten()
}

/// What native expression `id` is as a virtual interface value: `None` for
/// `null`, the type of a virtual interface, of the interface instance it
/// names or of the result of the function it calls, or an error naming what
/// else it is.
fn native_value_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Result<Option<VirtualInterface>, String> {
    let id = expression_binding::ungroup(id, native);
    match &native.expression(id).kind {
        celox_sv_syntax::ExprKind::Literal(text) if text == "null" => return Ok(None),
        celox_sv_syntax::ExprKind::Name(name) if name.spelling == "null" => return Ok(None),
        celox_sv_syntax::ExprKind::Call { expression, .. } => {
            // An element removed from a queue of virtual interfaces.
            if let celox_sv_syntax::ExprKind::Member {
                expression: receiver,
                separator,
                name,
            } = &native
                .expression(expression_binding::ungroup(*expression, native))
                .kind
                && separator == "."
                && matches!(
                    instances::normalize_identifier(&name.spelling).as_str(),
                    "pop_front" | "pop_back"
                )
                && let Ok(Some(container)) =
                    containers::native_type(*receiver, native, tree, session, dimensions)
                && let Some(element) = semantic_types::of_type(
                    &mut session.types,
                    &tree.functions,
                    &container.element,
                    &dimensions.const_env,
                )
                .and_then(|element| of_type(&session.types, element))
            {
                return Ok(Some(element.as_ref().clone()));
            }
            let name =
                match expression_binding::object_call_method(tree, session, id, native, dimensions)
                {
                    Ok(Some((name, _, _))) => name,
                    _ => expression_binding::callable_name(tree, *expression, native)
                        .ok_or_else(|| "the call".to_owned())?,
                };
            return match dimensions
                .function_return_types
                .get(&name)
                .and_then(|result| result.semantic_type)
                .and_then(|ty| of_type(&session.types, ty))
            {
                Some(ty) => Ok(Some(ty.as_ref().clone())),
                None => Err(format!("a call of `{name}`")),
            };
        }
        // A class method called without parentheses (13.5.5).
        celox_sv_syntax::ExprKind::Member { .. }
            if let Ok(Some((name, _, _))) =
                expression_binding::object_method(tree, session, id, native, dimensions) =>
        {
            return match dimensions
                .function_return_types
                .get(&name)
                .and_then(|result| result.semantic_type)
                .and_then(|ty| of_type(&session.types, ty))
            {
                Some(ty) => Ok(Some(ty.as_ref().clone())),
                None => Err(format!("a call of `{name}`")),
            };
        }
        _ => {}
    }
    if let Some(ty) = native_type(tree, session, id, native, dimensions) {
        return Ok(Some(ty.as_ref().clone()));
    }
    let handle = interface_binding::actual_handle_key(
        id,
        tree,
        session,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )
    .ok();
    if let Some(handle) = &handle {
        let imported = scope::imported(tree);
        if let Some(interface) = imported.interface_types.get(handle) {
            // A port declared with a modport is a view of the instance
            // restricted to it (IEEE 1800-2023 25.5).
            return Ok(Some(VirtualInterface {
                interface: interface.clone(),
                modport: imported.interface_modports.get(handle).cloned(),
            }));
        }
    }
    Err(match handle {
        Some(handle) => format!("`{handle}`"),
        None => "the value".to_owned(),
    })
}

/// Check that native expression `id` may be assigned to a virtual interface
/// of type `target` (IEEE 1800-2023 25.9).
pub(super) fn check_assignment(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
    target: &VirtualInterface,
) -> Converted<()> {
    // The packages are analyzed once to find the interfaces before their
    // declarations exist; the analysis that has them checks the values, as
    // it checks the types (see `bind_type`).
    if declarations(session).is_none() {
        return Ok(());
    }
    // Each branch of a conditional may be the value.
    if let celox_sv_syntax::ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        check_assignment(tree, session, *then_expression, native, dimensions, target)?;
        return check_assignment(tree, session, *else_expression, native, dimensions, target);
    }
    // The value is typed here, as an interface instance has no expression
    // type; the type table decides (see `assignments::check_types`).
    match native_value_type(tree, session, id, native, dimensions) {
        Ok(None) => Ok(()),
        Ok(Some(source)) => {
            let source = source.type_id(&mut session.types);
            let target = target.type_id(&mut session.types);
            assignments::check_types(tree, session, dimensions, source, target, &|| {
                "assignment".into()
            })
        }
        Err(actual) => Err(AnalyzerError::IncompatibleVirtualInterface {
            context: "assignment".into(),
            actual,
            target: target.spelling(),
        }),
    }
}

/// The error for operator `op` applied to a virtual interface of type `ty`,
/// which is only assigned and compared (IEEE 1800-2023 25.9).
pub(super) fn arithmetic(op: &str, ty: &VirtualInterface) -> AnalyzerError {
    AnalyzerError::IncompatibleVirtualInterface {
        context: format!("operator `{op}`"),
        actual: "an operand".into(),
        target: ty.spelling(),
    }
}

/// Check that native expression `id` may be assigned to an unpacked array
/// of virtual interfaces of type `target`: an array variable of a virtual
/// interface type that `target` accepts (IEEE 1800-2023 25.9, 7.6).
pub(super) fn check_array_assignment(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
    target: &VirtualInterface,
) -> Converted<()> {
    let actual = match &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        celox_sv_syntax::ExprKind::Conditional {
            then_expression,
            else_expression,
            ..
        } => {
            check_array_assignment(tree, session, *then_expression, native, dimensions, target)?;
            return check_array_assignment(
                tree,
                session,
                *else_expression,
                native,
                dimensions,
                target,
            );
        }
        celox_sv_syntax::ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            match dimensions.get(&name) {
                Some(shape) => match shape
                    .type_id(&mut session.types, &tree.functions, &dimensions.const_env)
                    .and_then(|ty| element_of_array(&session.types, ty))
                    .or_else(|| {
                        shape
                            .type_id(&mut session.types, &tree.functions, &dimensions.const_env)
                            .and_then(|ty| of_type(&session.types, ty))
                    }) {
                    Some(source) => {
                        let source_type = source.type_id(&mut session.types);
                        let target_type = target.type_id(&mut session.types);
                        if session.types.assignable(source_type, target_type).is_ok() {
                            return Ok(());
                        }
                        format!("an array of `{}`", source.spelling())
                    }
                    None => format!("`{name}`"),
                },
                None => return Ok(()),
            }
        }
        // Patterns and other forms are checked element by element.
        _ => return Ok(()),
    };
    Err(AnalyzerError::IncompatibleVirtualInterface {
        context: "assignment".into(),
        actual,
        target: format!("an array of `{}`", target.spelling()),
    })
}

/// The interface of each value that native expression `id` may be in an
/// equality: `None` for `null`, or the interface; each branch of a
/// conditional counts.
fn equality_operand(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
    interface: &str,
) -> Result<(), String> {
    if let celox_sv_syntax::ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        equality_operand(
            tree,
            session,
            *then_expression,
            native,
            dimensions,
            interface,
        )?;
        return equality_operand(
            tree,
            session,
            *else_expression,
            native,
            dimensions,
            interface,
        );
    }
    match native_value_type(tree, session, id, native, dimensions)? {
        None => Ok(()),
        Some(source) if source.interface == interface => Ok(()),
        Some(source) => Err(format!("a `{}`", source.spelling())),
    }
}

/// Check the operands of an equality, one of which is a virtual interface:
/// the other is `null`, a virtual interface or an interface instance of the
/// same interface (IEEE 1800-2023 25.9).
pub(super) fn check_equality(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    left: celox_sv_syntax::ExprId,
    right: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    if declarations(session).is_none() {
        return Ok(());
    }
    for (one, other) in [(left, right), (right, left)] {
        let Some(ty) = native_type(tree, session, one, native, dimensions) else {
            continue;
        };
        let Err(actual) = equality_operand(tree, session, other, native, dimensions, &ty.interface)
        else {
            return Ok(());
        };
        return Err(AnalyzerError::IncompatibleVirtualInterface {
            context: "equality".into(),
            actual,
            target: ty.spelling(),
        });
    }
    Ok(())
}

/// An error if virtual interface `handle` is `null`.
pub(super) fn null_check(handle: &Expr) -> Expr {
    null_access(handle, 1, false)
}

/// Member `member` of the interface instance that virtual interface
/// `handle` of type `ty` denotes, as an event expression watches it: a
/// selection among the instances, which changes when `handle` does, and
/// zero for `null`.
pub(super) fn watched(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<Expr> {
    let functions = &tree.functions;
    let (found, signals) = self::member(session, ty, member)?;
    let width = casts::expr_type_from_type(functions, &found.r#type, &ConstEnv::default())
        .map(|shape| shape.width)
        .ok_or_else(|| unsupported(format!("width of virtual interface member `{member}`")))?;
    let mut value = Expr::Literal(format!("{width}'d0"));
    for (id, signal) in signals.into_iter().rev() {
        value = Expr::Mux {
            condition: Box::new(denotes(&handle, &id)),
            then_expr: Box::new(Expr::Ident(signal)),
            else_expr: Box::new(value),
        };
    }
    Ok(value)
}

/// The member signals that a watched member `handle.member` may be, each
/// with the condition that `handle` denotes its instance. An event item on
/// one counts each write of the member, which the settled selection
/// ([`watched`]) misses when a process pulses it.
pub(super) fn watched_writes(
    state: &Session,
    handle: &Expr,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<Vec<(Expr, Expr)>> {
    let (_, signals) = self::member(state, ty, member)?;
    Ok(signals
        .into_iter()
        .map(|(id, signal)| (Expr::Ident(signal), denotes(handle, &id)))
        .collect())
}

/// Whether virtual interface `handle` is `null`.
pub(super) fn is_null(handle: &Expr) -> Expr {
    Expr::Binary {
        left: Box::new(handle.clone()),
        op: BinaryOp::Eq,
        right: Box::new(Expr::Literal(format!("{WIDTH}'d0"))),
    }
}

/// `stmts`, the statements of an event control through virtual interfaces,
/// as one statement that [`without_event_blocks`] recognizes: a loop that
/// runs them once, starting with the `null` checks of the handles.
pub(super) fn event_block(mut stmts: Vec<Stmt>) -> Stmt {
    stmts.push(Stmt::Break);
    Stmt::Loop {
        kind: crate::procedural::LoopKind::EventBlock,
        init: Vec::new(),
        condition: None,
        step: Vec::new(),
        body: stmts,
    }
}

/// Whether `stmt` is an [`event_block`].
fn is_event_block(stmt: &Stmt) -> bool {
    matches!(
        stmt,
        Stmt::Loop {
            kind: crate::procedural::LoopKind::EventBlock,
            ..
        }
    )
}

/// `stmt` without the [`event_block`]s it contains, whose reads are those
/// of its timing controls, for the reads of `@*` (IEEE 1800-2023 9.4.2.2).
pub(super) fn without_event_blocks(stmt: &Stmt) -> Stmt {
    let all = |stmts: &[Stmt]| {
        stmts
            .iter()
            .filter(|stmt| !is_event_block(stmt))
            .map(without_event_blocks)
            .collect::<Vec<_>>()
    };
    match stmt {
        stmt if is_event_block(stmt) => Stmt::If {
            condition: Expr::Literal("1'b0".into()),
            then_body: Vec::new(),
            else_body: Vec::new(),
        },
        Stmt::If {
            condition,
            then_body,
            else_body,
        } => Stmt::If {
            condition: condition.clone(),
            then_body: all(then_body),
            else_body: all(else_body),
        },
        Stmt::Case {
            kind,
            selector,
            items,
            default,
        } => Stmt::Case {
            kind: *kind,
            selector: selector.clone(),
            items: items
                .iter()
                .map(|item| crate::procedural::CaseItemBase {
                    labels: item.labels.clone(),
                    body: all(&item.body),
                })
                .collect(),
            default: default.as_deref().map(all),
        },
        Stmt::Loop {
            kind,
            init,
            condition,
            step,
            body,
        } => Stmt::Loop {
            kind: kind.clone(),
            init: all(init),
            condition: condition.clone(),
            step: all(step),
            body: all(body),
        },
        Stmt::Fork { join, branches } => Stmt::Fork {
            join: *join,
            branches: branches.iter().map(|branch| all(branch)).collect(),
        },
        Stmt::Block { id, body } => Stmt::Block {
            id: *id,
            body: all(body),
        },
        stmt => stmt.clone(),
    }
}

/// The handle and the arms of `expr` when it is a member read of
/// [`read`]: a selection by id that ends in the `null` check.
fn read_chain(expr: &Expr) -> Option<(&Expr, Vec<(&Expr, &Expr)>)> {
    let mut arms = Vec::new();
    let mut handle = None;
    let mut current = expr;
    loop {
        match current {
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                let Expr::Binary {
                    left,
                    op: BinaryOp::Eq,
                    right,
                } = &**condition
                else {
                    return None;
                };
                // An instance id: a number, or the placeholder of one.
                let id = match &**right {
                    Expr::Literal(literal) => {
                        literal
                            .strip_prefix(&format!("{WIDTH}'d"))?
                            .parse::<u32>()
                            .ok()?;
                        &**right
                    }
                    Expr::Ident(name)
                        if matches!(
                            VirtualInterfaceSignal::of(name),
                            Some(VirtualInterfaceSignal::Instance { .. })
                        ) =>
                    {
                        &**right
                    }
                    _ => return None,
                };
                if handle.is_some_and(|handle| handle != &**left) {
                    return None;
                }
                handle = Some(&**left);
                arms.push((id, &**then_expr));
                current = else_expr;
            }
            Expr::Call {
                callee:
                    Callee::Checked {
                        check: system_functions::AccessCheck::VirtualInterface,
                        ..
                    },
                args,
                ..
            } => {
                let Some(Some(Expr::Binary { left, .. })) = args.first() else {
                    return None;
                };
                if handle.is_some_and(|handle| handle != &**left) {
                    return None;
                }
                return Some((&**left, arms));
            }
            _ => return None,
        }
    }
}

/// The handles that `expr` reads members through, each once.
pub(super) fn read_handles(expr: &Expr, handles: &mut Vec<Expr>) {
    if let Some((handle, arms)) = read_chain(expr) {
        if !handles.contains(handle) {
            handles.push(handle.clone());
        }
        for (_, arm) in arms {
            read_handles(arm, handles);
        }
        return;
    }
    for child in children(expr) {
        read_handles(child, handles);
    }
}

/// The operands of `expr` (not those of its constant selects).
pub(super) fn children(expr: &Expr) -> Vec<&Expr> {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => Vec::new(),
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            vec![expr]
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => parts.iter().collect(),
        Expr::Binary { left, right, .. } => vec![left, right],
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => vec![condition, then_expr, else_expr],
        Expr::Call { args, .. } => args.iter().flatten().collect(),
        Expr::Inside { expr, items } => std::iter::once(&**expr)
            .chain(items.iter().flat_map(|item| item.exprs()))
            .collect(),
    }
}

/// `expr` with `map` applied to its operands (not those of its constant
/// selects).
pub(super) fn map_children(expr: Expr, map: &mut dyn FnMut(Expr) -> Expr) -> Expr {
    let mut boxed = |expr: Box<Expr>| Box::new(map(*expr));
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => expr,
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: boxed(expr),
            msb,
            lsb,
            signed,
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: boxed(expr),
            width,
            signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op,
            expr: boxed(expr),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: boxed(left),
            op,
            right: boxed(right),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: boxed(condition),
            then_expr: boxed(then_expr),
            else_expr: boxed(else_expr),
        },
        Expr::Concat(parts) => Expr::Concat(parts.into_iter().map(&mut *map).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count,
            parts: parts.into_iter().map(&mut *map).collect(),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            callee,
            args: args.into_iter().map(|arg| arg.map(&mut *map)).collect(),
            arg_order,
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(map(*expr)),
            items: items
                .into_iter()
                .map(|item| match item {
                    InsideItem::Value(value) => InsideItem::Value(map(value)),
                    InsideItem::Range { low, high } => InsideItem::Range {
                        low: map(low),
                        high: map(high),
                    },
                })
                .collect(),
        },
    }
}

/// The interface of placeholder signal `name`, if it is one: a member or the
/// id of the instance an access selects (see [`member`]).
pub(crate) fn placeholder_interface(name: &str) -> Option<String> {
    match VirtualInterfaceSignal::of(name)? {
        VirtualInterfaceSignal::Placeholder { interface, .. }
        | VirtualInterfaceSignal::Instance { interface } => Some(interface),
        _ => None,
    }
}

/// Whether `name` is a placeholder signal (see [`member_signals`]).
pub(crate) fn is_placeholder_signal(name: &str) -> bool {
    placeholder_interface(name).is_some()
}
