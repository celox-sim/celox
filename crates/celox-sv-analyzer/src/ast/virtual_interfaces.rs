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

/// A virtual interface type: an interface, with a modport if one is
/// selected (IEEE 1800-2023 25.9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VirtualInterface {
    pub interface: String,
    pub modport: Option<String>,
}

impl VirtualInterface {
    /// Whether a value of type `source` may be assigned to a variable of this
    /// type: the same interface, and a modport only from no modport or the
    /// same one (IEEE 1800-2023 25.9).
    pub(super) fn accepts(&self, source: &VirtualInterface) -> bool {
        self.interface == source.interface
            && (source.modport.is_none() || source.modport == self.modport)
    }

    /// How the type is spelled in source.
    pub(super) fn spelling(&self) -> String {
        match &self.modport {
            Some(modport) => format!("virtual {}.{modport}", self.interface),
            None => format!("virtual {}", self.interface),
        }
    }
}

/// The interfaces that virtual interface types of a design name, and the
/// instances of each. The frontend builds it: the declarations from the
/// interface signatures, the instance ids from the elaborated hierarchy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualInterfaces {
    pub interfaces: HashMap<String, VirtualInterfaceDeclaration>,
    /// The instances are those of the elaborated design, not yet unknown.
    pub elaborated: bool,
}

/// An interface that a virtual interface type names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualInterfaceDeclaration {
    /// The ports and the variables and nets the interface declares.
    pub members: Vec<VirtualInterfaceMember>,
    /// Each modport with the members it lists and their directions.
    pub modports: HashMap<String, Vec<(String, String)>>,
    /// The ids of the instances of the interface in the elaborated design,
    /// the values a virtual interface of it may hold besides `null`.
    pub instances: Vec<u32>,
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
        for prototype in
            interfaces::interface_prototypes_with_options(sources, Some(&names), options.clone())?
        {
            let module = &prototype.module;
            // Member bounds may name the interface's constants.
            let constants = module
                .parameters()
                .iter()
                .filter_map(|parameter| {
                    Some((parameter.name().to_owned(), parameter.resolved_value()?))
                })
                .collect::<HashMap<_, _>>();
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
                    instances: Vec::new(),
                    parameterized: !prototype.parameters.is_empty(),
                },
            );
        }
        Ok(Some(Arc::new(declarations)))
    }
}

/// `r#type` of a member of a non-parameterized interface, whose bounds are
/// constants, possibly the interface's `constants`.
fn resolved_type(r#type: &crate::ir::Type, constants: &HashMap<String, i128>) -> Option<Type> {
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
    Some(Type::resolved(
        kind,
        r#type.is_signed(),
        r#type.signed_element_depth(),
        &packed,
        &[],
    ))
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
}

impl VirtualInterfaceSignal {
    /// The kind of a signal named `name`, if the frontend binds it.
    pub fn of(name: &str) -> Option<Self> {
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

    pub(super) fn name(&self) -> String {
        match self {
            Self::Member { id, member } => format!("{MEMBER_PREFIX}{id}${member}"),
            Self::Identity { handle } => format!("{IDENTITY_PREFIX}{handle}"),
            Self::Template { interface, member } => {
                format!("{TEMPLATE_PREFIX}{interface}${member}")
            }
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
pub(super) fn declarations(state: &AnalysisState) -> Option<&VirtualInterfaces> {
    state.virtual_interfaces.as_deref()
}

/// The type `virtual name[.modport]`, checked against the interface.
pub(super) fn bind_type(
    state: &AnalysisState,
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
    let declaration = declarations
        .interfaces
        .get(&interface)
        .ok_or_else(|| unsupported(format!("virtual interface type of `{interface}`")))?;
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
/// the signals that denote it in each instance the value may denote.
pub(super) fn member<'a>(
    state: &'a AnalysisState,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<(&'a VirtualInterfaceMember, Vec<(u32, String)>)> {
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
    let signals = declaration
        .instances
        .iter()
        .map(|&id| {
            (
                id,
                VirtualInterfaceSignal::Member {
                    id,
                    member: member.to_owned(),
                }
                .name(),
            )
        })
        .collect();
    Ok((found, signals))
}

/// An error if member `member` of a virtual interface of type `ty` is an
/// input of its modport, which may not be assigned (IEEE 1800-2023 25.5,
/// 23.3.3.4).
pub(super) fn check_write(
    state: &AnalysisState,
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

/// The signals that denote the members of every interface instance that a
/// virtual interface may denote, which each scope declares.
pub(super) fn member_signals(declarations: &VirtualInterfaces) -> Vec<Signal> {
    let mut interfaces = declarations.interfaces.iter().collect::<Vec<_>>();
    interfaces.sort_by(|left, right| left.0.cmp(right.0));
    let templates = interfaces.iter().flat_map(|(interface, declaration)| {
        declaration.members.iter().map(|member| Signal {
            name: template(interface, &member.name),
            r#type: member.r#type.clone(),
            is_net: false,
            unresolved_net: false,
            is_const: false,
            package_variable: None,
            hierarchical: None,
        })
    });
    let templates = templates.collect::<Vec<_>>();
    templates
        .into_iter()
        .chain(interfaces.into_iter().flat_map(|(_, declaration)| {
            declaration.instances.iter().flat_map(|&id| {
                declaration.members.iter().map(move |member| {
                    let name = VirtualInterfaceSignal::Member {
                        id,
                        member: member.name.clone(),
                    }
                    .name();
                    // It denotes the member of another instance, as a
                    // hierarchical name does: that instance's object, not
                    // one of this module.
                    Signal {
                        hierarchical: Some(name.clone()),
                        name,
                        r#type: member.r#type.clone(),
                        is_net: member.is_net,
                        unresolved_net: false,
                        is_const: false,
                        package_variable: None,
                    }
                })
            })
        }))
        .collect()
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
        .then(|| native_member(tree, base, native, dimensions))
        .flatten()
}

/// `selected`, a select bound against the [`template`] of member `member`,
/// read from the instance that virtual interface `handle` of type `ty`
/// denotes; an error for `null`.
pub(super) fn selected_read(
    state: &AnalysisState,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
    selected: Expr,
    dimensions: &PackedDimensions,
) -> Converted<Expr> {
    let (_, signals) = self::member(state, ty, member)?;
    check_pure(&handle)?;
    let width = expr_static_width(state, &selected, dimensions)
        .ok_or_else(|| unsupported(format!("width of a select of `{member}`")))?;
    let signed = dimensions::expression_signedness(&selected, dimensions, &dimensions.const_env)
        .unwrap_or(false);
    let from = template(&ty.interface, member);
    let mut value = null_access(&handle, width, signed);
    for (id, signal) in signals.into_iter().rev() {
        value = Expr::Mux {
            condition: Box::new(denotes(&handle, id)),
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

/// The virtual interface type of native expression `id`, judged before it
/// is bound: a variable, a property, or an element of an array or a
/// container of virtual interfaces.
pub(super) fn native_type(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Option<Arc<VirtualInterface>> {
    declarations(&tree.state)?;
    let id = expression_binding::ungroup(id, native);
    if let celox_sv_syntax::ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(id).kind
        && matches!(dimension.kind, celox_sv_syntax::DimensionKind::Index(_))
        && let Ok(Some(container)) = containers::native_type(*expression, native, tree, dimensions)
    {
        return container.element.virtual_interface.clone();
    }
    // A variable or package variable `p::v`, or an element of an array of
    // them that indexes each of its unpacked dimensions.
    let mut base = id;
    let mut indices = 0;
    while let celox_sv_syntax::ExprKind::Select {
        expression,
        dimension,
    } = &native.expression(base).kind
        && matches!(dimension.kind, celox_sv_syntax::DimensionKind::Index(_))
    {
        base = expression_binding::ungroup(*expression, native);
        indices += 1;
    }
    let key = match &native.expression(base).kind {
        celox_sv_syntax::ExprKind::Name(name) => {
            Some(instances::normalize_identifier(&name.spelling))
        }
        celox_sv_syntax::ExprKind::Member {
            expression,
            separator,
            name,
        } if separator == "::" => match &native
            .expression(expression_binding::ungroup(*expression, native))
            .kind
        {
            celox_sv_syntax::ExprKind::Name(package) => Some(format!(
                "{}::{}",
                instances::normalize_identifier(&package.spelling),
                instances::normalize_identifier(&name.spelling)
            )),
            _ => None,
        },
        _ => None,
    };
    if let Some(shape) = key.and_then(|key| dimensions.get(&key)) {
        return shape
            .virtual_interface
            .clone()
            .filter(|_| shape.unpacked.len() == indices && shape.container.is_none());
    }
    if indices > 0 {
        return None;
    }
    let (property, _) = expression_binding::object_property(tree, id, native, dimensions)
        .ok()
        .flatten()?;
    property
        .r#type
        .virtual_interface
        .clone()
        .filter(|_| property.r#type.container.is_none())
}

/// The member access `h.m` that native expression `id` is, through a
/// virtual interface `h`: the native handle, its type and the member.
pub(super) fn native_member(
    tree: &AnalysisContext<'_>,
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
    let ty = native_type(tree, *expression, native, dimensions)?;
    Some((
        *expression,
        ty,
        instances::normalize_identifier(&name.spelling),
    ))
}

/// The comparison of a virtual interface value with the id of an instance.
fn denotes(handle: &Expr, id: u32) -> Expr {
    Expr::Binary {
        left: Box::new(handle.clone()),
        op: BinaryOp::Eq,
        right: Box::new(Expr::Literal(format!("{WIDTH}'d{id}"))),
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
    state: &AnalysisState,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
    dimensions: &PackedDimensions,
    select: impl Fn(Expr) -> Converted<Expr>,
) -> Converted<Expr> {
    let (found, signals) = self::member(state, ty, member)?;
    check_pure(&handle)?;
    let mut arms = Vec::new();
    for (id, signal) in signals {
        arms.push((id, select(Expr::Ident(signal))?));
    }
    let (width, signed) = match arms.first() {
        Some((_, value)) => (
            expr_static_width(state, value, dimensions),
            dimensions::expression_signedness(value, dimensions, &dimensions.const_env)
                .unwrap_or(false),
        ),
        None => {
            let shape = casts::expr_type_from_type(state, &found.r#type, &HashMap::default());
            (shape.map(|shape| shape.width), found.r#type.is_signed())
        }
    };
    let width = width
        .ok_or_else(|| unsupported(format!("width of virtual interface member `{member}`")))?;
    let mut value = null_access(&handle, width, signed);
    for (id, arm) in arms.into_iter().rev() {
        value = Expr::Mux {
            condition: Box::new(denotes(&handle, id)),
            then_expr: Box::new(arm),
            else_expr: Box::new(value),
        };
    }
    Ok(value)
}

/// An error unless evaluating virtual interface `handle` has no effects: a
/// member access compares it with each instance, evaluating it again.
fn check_pure(handle: &Expr) -> Converted<()> {
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
    if effect {
        return Err(unsupported(
            "virtual interface handle expression with a call that may have effects",
        ));
    }
    Ok(())
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
    state: &AnalysisState,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
    mut arm: impl FnMut(String) -> Converted<Vec<Stmt>>,
) -> Converted<Stmt> {
    let (_, signals) = self::member(state, ty, member)?;
    check_pure(&handle)?;
    let items = signals
        .into_iter()
        .map(|(id, signal)| {
            Ok(crate::procedural::CaseItemBase {
                labels: vec![crate::procedural::CaseLabel::Value(Expr::Literal(format!(
                    "{WIDTH}'d{id}"
                )))],
                body: arm(signal)?,
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
pub(super) fn identity_signal(
    state: &AnalysisState,
    handle: &str,
    interface: &str,
) -> Option<Signal> {
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
        is_const: false,
        package_variable: None,
        hierarchical: None,
    })
}

/// [`identity_signal`] for a port of interface `interface`, unless the
/// interface declares no members: the frontend finds the instance a port
/// denotes by the connections of its members.
pub(super) fn port_identity_signal(
    state: &AnalysisState,
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
    name: &str,
    dimensions: &PackedDimensions,
) -> Converted<Option<Expr>> {
    if declarations(&tree.state).is_none() || dimensions.get(name).is_some() {
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
    match scope::imported(&tree.state).interface_types.get(name) {
        Some(interface) => Err(unsupported(format!(
            "virtual interface value of port `{name}` of interface `{interface}`, which declares no members"
        ))),
        None => Ok(None),
    }
}

/// The interface instances and ports of a scope with symbols `symbols` and
/// ports `ports` that a virtual interface may denote.
pub(super) fn handles(
    state: &AnalysisState,
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
    id: celox_sv_syntax::ExprId,
    dimensions: &PackedDimensions,
) -> Option<Expr> {
    declarations(&tree.state)?;
    let handle = interface_binding::actual_handle_key(
        id,
        tree,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )
    .ok()?;
    identity(tree, &handle, dimensions).ok().flatten()
}

/// What native expression `id` is as a virtual interface value: `None` for
/// `null`, the type of a virtual interface, of the interface instance it
/// names or of the result of the function it calls, or an error naming what
/// else it is.
fn native_value_type(
    tree: &AnalysisContext<'_>,
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
                    containers::native_type(*receiver, native, tree, dimensions)
                && let Some(element) = &container.element.virtual_interface
            {
                return Ok(Some(element.as_ref().clone()));
            }
            let name =
                match expression_binding::object_method(tree, *expression, native, dimensions) {
                    Ok(Some((name, _, _))) => name,
                    _ => expression_binding::callable_name(&tree.state, *expression, native)
                        .ok_or_else(|| "the call".to_owned())?,
                };
            return match dimensions.function_return_types.get(&name) {
                Some(result) => match &result.virtual_interface {
                    Some(ty) => Ok(Some(ty.as_ref().clone())),
                    None => Err(format!("a call of `{name}`")),
                },
                None => Err(format!("a call of `{name}`")),
            };
        }
        _ => {}
    }
    if let Some(ty) = native_type(tree, id, native, dimensions) {
        return Ok(Some(ty.as_ref().clone()));
    }
    let handle = interface_binding::actual_handle_key(
        id,
        tree,
        &dimensions.const_env,
        &dimensions.type_aliases,
    )
    .ok();
    if let Some(handle) = &handle {
        let imported = scope::imported(&tree.state);
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
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
    target: &VirtualInterface,
) -> Converted<()> {
    // Each branch of a conditional may be the value.
    if let celox_sv_syntax::ExprKind::Conditional {
        then_expression,
        else_expression,
        ..
    } = &native
        .expression(expression_binding::ungroup(id, native))
        .kind
    {
        check_assignment(tree, *then_expression, native, dimensions, target)?;
        return check_assignment(tree, *else_expression, native, dimensions, target);
    }
    let actual = match native_value_type(tree, id, native, dimensions) {
        Ok(None) => return Ok(()),
        Ok(Some(source)) if target.accepts(&source) => return Ok(()),
        Ok(Some(source)) => format!("a `{}`", source.spelling()),
        Err(actual) => actual,
    };
    Err(AnalyzerError::IncompatibleVirtualInterface {
        context: "assignment".into(),
        actual,
        target: target.spelling(),
    })
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

/// Check that a formal of virtual interface type `formal` may be copied out
/// to the actual argument, native expression `id` (IEEE 1800-2023 13.5,
/// 25.9).
pub(super) fn check_copy_out(
    tree: &AnalysisContext<'_>,
    id: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
    formal: &VirtualInterface,
) -> Converted<()> {
    let actual = match native_type(tree, id, native, dimensions) {
        Some(actual) if actual.accepts(formal) => return Ok(()),
        Some(actual) => actual.spelling(),
        None => "the argument".to_owned(),
    };
    Err(AnalyzerError::IncompatibleVirtualInterface {
        context: "output argument".into(),
        actual: format!("a `{}`", formal.spelling()),
        target: actual,
    })
}

/// Check that native expression `id` may be assigned to an unpacked array
/// of virtual interfaces of type `target`: an array variable of a virtual
/// interface type that `target` accepts (IEEE 1800-2023 25.9, 7.6).
pub(super) fn check_array_assignment(
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
            check_array_assignment(*then_expression, native, dimensions, target)?;
            return check_array_assignment(*else_expression, native, dimensions, target);
        }
        celox_sv_syntax::ExprKind::Name(name) => {
            let name = instances::normalize_identifier(&name.spelling);
            match dimensions.get(&name) {
                Some(shape) => match shape.virtual_interface.as_deref() {
                    Some(source) if target.accepts(source) => return Ok(()),
                    Some(source) => format!("an array of `{}`", source.spelling()),
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
        equality_operand(tree, *then_expression, native, dimensions, interface)?;
        return equality_operand(tree, *else_expression, native, dimensions, interface);
    }
    match native_value_type(tree, id, native, dimensions)? {
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
    left: celox_sv_syntax::ExprId,
    right: celox_sv_syntax::ExprId,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Converted<()> {
    if declarations(&tree.state).is_none() {
        return Ok(());
    }
    for (one, other) in [(left, right), (right, left)] {
        let Some(ty) = native_type(tree, one, native, dimensions) else {
            continue;
        };
        let Err(actual) = equality_operand(tree, other, native, dimensions, &ty.interface) else {
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
    state: &AnalysisState,
    handle: Expr,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<Expr> {
    let (found, signals) = self::member(state, ty, member)?;
    let width = casts::expr_type_from_type(state, &found.r#type, &HashMap::default())
        .map(|shape| shape.width)
        .ok_or_else(|| unsupported(format!("width of virtual interface member `{member}`")))?;
    let mut value = Expr::Literal(format!("{width}'d0"));
    for (id, signal) in signals.into_iter().rev() {
        value = Expr::Mux {
            condition: Box::new(denotes(&handle, id)),
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
    state: &AnalysisState,
    handle: &Expr,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<Vec<(Expr, Expr)>> {
    let (_, signals) = self::member(state, ty, member)?;
    Ok(signals
        .into_iter()
        .map(|(id, signal)| (Expr::Ident(signal), denotes(handle, id)))
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
        kind: crate::procedural::LoopKind::Forever,
        init: Vec::new(),
        condition: None,
        step: Vec::new(),
        body: stmts,
    }
}

/// Whether `stmt` is an [`event_block`].
fn is_event_block(stmt: &Stmt) -> bool {
    matches!(stmt, Stmt::Loop {
        kind: crate::procedural::LoopKind::Forever,
        body,
        ..
    } if matches!(body.first(), Some(Stmt::Eval(Expr::Call {
        callee: Callee::Checked {
            check: system_functions::AccessCheck::VirtualInterface,
            ..
        },
        ..
    }))))
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
        stmt => stmt.clone(),
    }
}

/// The handle and the arms of `expr` when it is a member read of
/// [`read`]: a selection by id that ends in the `null` check.
fn read_chain(expr: &Expr) -> Option<(&Expr, Vec<(u32, &Expr)>)> {
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
                let Expr::Literal(literal) = &**right else {
                    return None;
                };
                let id = literal.strip_prefix(&format!("{WIDTH}'d"))?.parse().ok()?;
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
fn children(expr: &Expr) -> Vec<&Expr> {
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
fn map_children(expr: Expr, map: &mut dyn FnMut(Expr) -> Expr) -> Expr {
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
