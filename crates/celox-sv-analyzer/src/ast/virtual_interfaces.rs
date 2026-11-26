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
/// The prefix of the signal that stands for static variable `<name>` of a
/// subroutine of whichever instance of interface `<interface>` a call
/// through a virtual interface selects: `$vifstatic$<interface>$<name>`.
const STATIC_PLACEHOLDER_PREFIX: &str = "$vifstatic$";
/// The prefix of that variable of interface instance `<id>`:
/// `$vifstaticof$<id>$<name>`.
const STATIC_PREFIX: &str = "$vifstaticof$";
/// The prefix of the signal that an outward hierarchical name of the body of
/// an interface instance denotes (IEEE 1800-2023 23.8), see
/// [`VirtualInterfaceSignal::Upward`].
const UPWARD_PREFIX: &str = "$vifup$";

/// A virtual interface type: an interface, with a modport if one is
/// selected (IEEE 1800-2023 25.9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VirtualInterface {
    pub interface: String,
    pub modport: Option<String>,
    /// The values of the value parameters of a parameterized interface, in
    /// the order it declares them, defaults included: part of the type
    /// (25.9). Empty for an interface without parameters.
    pub arguments: Arc<[i128]>,
}

impl VirtualInterface {
    /// The type `virtual interface[.modport]` of an interface without
    /// parameters, or of a specialization with `arguments`.
    pub(super) fn new(interface: String, modport: Option<String>, arguments: Arc<[i128]>) -> Self {
        Self {
            interface,
            modport,
            arguments,
        }
    }

    /// The key of the specialization the type names (see
    /// [`specialization_key`]).
    pub(crate) fn key(&self) -> String {
        specialization_key(&self.interface, &self.arguments)
    }

    /// The semantic type of a virtual interface of this type.
    pub(super) fn type_id(&self, table: &mut semantic_types::TyTable) -> semantic_types::TyId {
        let interface = if self.arguments.is_empty() {
            table.interface(&self.interface)
        } else {
            table.interface_specialization(&self.interface, &self.arguments)
        };
        table.intern(semantic_types::TyData::VirtualInterface {
            interface,
            modport: self.modport.as_deref().map(Into::into),
        })
    }

    /// How the type is spelled in source, with the values of the
    /// parameters of a specialization.
    pub(super) fn spelling(&self) -> String {
        let mut spelling = format!("virtual {}", self.interface);
        if !self.arguments.is_empty() {
            spelling.push_str(&format!(
                "#({})",
                self.arguments
                    .iter()
                    .map(i128::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(modport) = &self.modport {
            spelling.push_str(&format!(".{modport}"));
        }
        spelling
    }
}

/// The key under which the declarations hold the specialization of
/// interface `interface` with parameter values `arguments`: the interface's
/// name when it has no parameters, and otherwise the name with a hash of
/// the values, as module specializations are keyed. Placeholders, the
/// subroutines called through virtual interfaces and the instances the
/// frontend numbers are per key, so a handle of one specialization never
/// selects an instance of another (25.9).
pub(crate) fn specialization_key(interface: &str, arguments: &[i128]) -> String {
    if arguments.is_empty() {
        return interface.to_owned();
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = fxhash::FxHasher64::default();
    arguments.hash(&mut hasher);
    format!("{interface}#{:016x}", hasher.finish())
}

/// The interfaces that virtual interface types of a design name, from the
/// interface signatures. Analysis does not know their instances, which the
/// frontend numbers in the elaborated hierarchy and repeats each access for
/// (see [`crate::ir::Module::expand_instances`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtualInterfaces {
    pub interfaces: HashMap<String, VirtualInterfaceDeclaration>,
    /// The parameter values of the specialization each parameter value
    /// assignment of an interface gives, by the interface and the
    /// assignment (see [`specialization`]).
    pub(crate) requests: HashMap<String, Arc<[i128]>>,
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
    /// The names of the formals of each task and function the interface
    /// declares, which a virtual interface calls (25.9).
    pub subroutines: HashMap<String, Vec<String>>,
    /// The DPI functions and tasks the interface imports (35.5).
    pub dpi_imports: HashSet<String>,
    /// The subroutines each modport imports (25.7), which a virtual
    /// interface of its type may call.
    pub modport_imports: HashMap<String, Vec<String>>,
    /// The name of the interface, which a specialization is keyed apart
    /// from by its parameter values.
    pub interface: String,
    /// The values of the parameters of a specialization, in the order the
    /// interface declares them; empty for the declaration of an interface
    /// without parameters.
    pub arguments: Arc<[i128]>,
    /// What specializing a parameterized interface needs.
    pub(crate) template: Option<Arc<DeclarationTemplate>>,
    /// The hierarchical names, by their components, that the tasks and
    /// functions of the interface use and it does not declare, which each
    /// instance finds from its parent (IEEE 1800-2023 23.8). Until they are
    /// resolved, the subroutines are not built, and a call of one is an
    /// error ([`crate::AnalyzerError::VirtualInterfaceOutward`]).
    pub outward: Vec<Vec<String>>,
}

/// What specializing a parameterized interface takes, which each
/// specialization's signature is analyzed from with its own values (25.9).
#[derive(Debug, Clone)]
pub(crate) struct DeclarationTemplate {
    /// The parameters a parameter value assignment may override, by the
    /// names the interface declares them by, in declaration order.
    overridable: Vec<String>,
    /// The interface's signature, analyzed again for each specialization,
    /// which evaluates its parameters as any instance's.
    source: Arc<interface_ast::SignatureSource>,
}

impl PartialEq for DeclarationTemplate {
    fn eq(&self, other: &Self) -> bool {
        self.overridable == other.overridable && Arc::ptr_eq(&self.source, &other.source)
    }
}

impl Eq for DeclarationTemplate {}

impl DeclarationTemplate {
    /// The specialization whose parameter value assignment gives
    /// `overrides`: the values of its overridable parameters, as their types
    /// convert them and in declaration order, and its members.
    pub(crate) fn specialize(
        &self,
        interface: &str,
        overrides: &HashMap<String, ConstExpr>,
    ) -> Converted<(Arc<[i128]>, Vec<VirtualInterfaceMember>)> {
        let (module, enumerations) = interface_ast::specialize(&self.source, overrides)?;
        let values = self
            .overridable
            .iter()
            .map(|name| {
                module
                    .parameters()
                    .iter()
                    .find(|parameter| declared_name(parameter.name()) == name)?
                    .resolved_value()
            })
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                unsupported(format!(
                    "virtual interface type of parameterized interface `{interface}` whose parameters are not integral constants"
                ))
            })?;
        Ok((values.into(), members_of(&module, &enumerations)))
    }

    /// The overridable parameters of the specialization whose values are
    /// `values`, by name.
    pub(crate) fn overrides(&self, values: &[i128]) -> HashMap<String, i128> {
        self.overridable
            .iter()
            .cloned()
            .zip(values.iter().copied())
            .collect()
    }
}

/// The members of the interface whose signature `module` is: its ports
/// `<port>$<member>`, typed with its constants, a member of an enumeration
/// type of `enumerations` with that type (IEEE 1800-2023 6.19.3).
fn members_of(
    module: &crate::ir::Module,
    enumerations: &[(String, Type)],
) -> Vec<VirtualInterfaceMember> {
    // Member bounds may name the interface's constants, and those of the
    // packages it uses, through their imports too (IEEE 1800-2023 26.3).
    let constants = module
        .imported_parameters()
        .iter()
        .chain(module.parameters())
        .filter_map(|parameter| Some((parameter.name().to_owned(), parameter.resolved_value()?)))
        .collect::<ConstEnv>();
    module
        .ports()
        .iter()
        .filter_map(|port| {
            let (_, name) = port.name().split_once('$')?;
            let enumeration = enumerations
                .iter()
                .find(|(member, _)| member == name)
                .map(|(_, r#type)| r#type.clone());
            Some(VirtualInterfaceMember {
                name: name.to_string(),
                r#type: match enumeration {
                    Some(r#type) => MemberType::Value(r#type),
                    None => resolved_type(port.r#type(), &constants)?,
                },
                is_net: port.is_net(),
            })
        })
        .collect()
}

/// The name an interface declares the parameter `name` of its signature by:
/// the signature's port prefixes it (see [`VirtualInterfaces::declarations_of`]).
fn declared_name(name: &str) -> &str {
    name.split_once('$').map_or(name, |(_, declared)| declared)
}

impl VirtualInterfaceDeclaration {
    /// Whether some members are objects that the subroutines' hierarchical
    /// names denote (see [`outward_member`]), which the packages do not
    /// declare placeholders for.
    pub(crate) fn has_outward_members(&self) -> bool {
        self.members
            .iter()
            .any(|member| member.name.starts_with(OUTWARD_MEMBER_PREFIX))
    }

    /// The declaration of the specialization whose overridable parameters
    /// have `values` and whose members are `members`.
    fn specialization(&self, values: &Arc<[i128]>, members: Vec<VirtualInterfaceMember>) -> Self {
        Self {
            members,
            arguments: values.clone(),
            template: None,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualInterfaceMember {
    pub name: String,
    pub(crate) r#type: MemberType,
    pub is_net: bool,
}

/// The type of a member of an interface that virtual interface types name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemberType {
    /// A type that names no class.
    Value(Type),
    /// A handle of class `0` (IEEE 1800-2023 8.4). A handle type names its
    /// class by its semantic type, which each reader binds in its own table:
    /// the declarations are made before any table.
    Handle(String),
}

impl VirtualInterfaceMember {
    /// The type of the member as the analyses of `design` read it, such as
    /// that of a signal which denotes the member of an instance.
    pub fn declared_type(&self, design: &mut super::packages::Design) -> Type {
        self.type_in(&mut design.types)
    }

    /// The type of the member, bound in `table` if it is a class handle.
    pub(super) fn type_in(&self, table: &mut semantic_types::TyTable) -> Type {
        match &self.r#type {
            MemberType::Value(ty) => ty.clone(),
            MemberType::Handle(class) => classes::handle(table, class),
        }
    }
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
                    interfaces::prototypes_with_packages(sources, Some(&names), options.clone())
                })?;
        for interfaces::Prototype {
            prototype,
            enumerations,
            source,
        } in prototypes
        {
            let module = &prototype.module;
            // The signature is a module with a port of the interface, whose
            // members are its ports `<port>$<member>`.
            let parameterized = !prototype.parameters.is_empty();
            // A parameterized interface's members are typed for each
            // specialization (25.9) with the constants of its parameters.
            let template = parameterized.then(|| {
                Arc::new(DeclarationTemplate {
                    overridable: prototype.parameters.clone(),
                    source: Arc::new(source),
                })
            });
            let members = members_of(module, &enumerations);
            let modport_imports = prototype
                .modports
                .iter()
                .map(|(name, items)| {
                    (
                        name.clone(),
                        items
                            .iter()
                            .filter(|(_, direction)| direction == "import")
                            .map(|(item, _)| item.clone())
                            .collect(),
                    )
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
                    parameterized,
                    subroutines: HashMap::default(),
                    dpi_imports: HashSet::default(),
                    modport_imports,
                    interface: prototype.name.clone(),
                    arguments: Arc::new([]),
                    template,
                    outward: Vec::new(),
                },
            );
        }
        for source in parsed {
            for (interface, subroutine, formals) in source.interface_subroutine_formals(&names) {
                let Some(declaration) = declarations.interfaces.get_mut(&interface) else {
                    continue;
                };
                match formals {
                    Some(formals) => {
                        declaration.subroutines.insert(subroutine, formals);
                    }
                    None => {
                        declaration.dpi_imports.insert(subroutine);
                    }
                }
            }
        }
        Ok(Some(Arc::new(declarations)))
    }
}

/// `r#type` of a member of a non-parameterized interface, whose bounds are
/// constants, possibly the interface's `constants`.
pub(crate) fn resolved_type(r#type: &crate::ir::Type, constants: &ConstEnv) -> Option<MemberType> {
    // A handle of a class object, such as the proxy through which a bus
    // functional model calls back the class that drives it (8.4).
    if r#type.kind() == crate::ir::TypeKind::Object
        && let Some(class) = r#type.class()
        && r#type.unpacked_ranges().is_empty()
        && r#type.container().is_none()
    {
        return Some(MemberType::Handle(class.to_owned()));
    }
    // A named event, the handle of a synchronization object (15.5).
    if r#type.kind() == crate::ir::TypeKind::Event
        && r#type.unpacked_ranges().is_empty()
        && r#type.container().is_none()
    {
        return Some(MemberType::Handle(events::CLASS.to_owned()));
    }
    // A string variable, a value of its own type (6.16).
    if r#type.kind() == crate::ir::TypeKind::String
        && r#type.unpacked_ranges().is_empty()
        && r#type.container().is_none()
    {
        return super::type_binding::builtin_type("string", None).map(MemberType::Value);
    }
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
    // A fixed-size unpacked array, such as a bus functional model's memory,
    // is a member whose elements the accesses select (7.4).
    let unpacked = r#type
        .unpacked_ranges()
        .iter()
        .map(|range| {
            range
                .resolved_bounds()
                .or_else(|| Some((eval(range.left())?, eval(range.right())?)))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(MemberType::Value(
        Type::resolved(
            kind,
            r#type.is_signed(),
            r#type.signed_element_depth(),
            &packed,
            &unpacked,
        )
        .with_chandle(r#type.is_chandle()),
    ))
}

/// The definition whose instances a placeholder selects among: an interface,
/// or a specialization of a module whose classes' overrides read its items
/// (see `crate::ir::ModuleClasses::key`). A placeholder holds it whole, as a
/// typed key of a fixed width, never the definition's name, which may hold
/// any character a placeholder separates its parts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefinitionId(u64);

impl DefinitionId {
    /// The key of the definition named `name`.
    pub fn of(name: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = fxhash::FxHasher64::default();
        name.hash(&mut hasher);
        Self(hasher.finish())
    }

    /// The key of hierarchical name `components` of the body of interface
    /// instance `handle`: of the parsed components, not of a spelling that
    /// joins them, which an escaped identifier could make ambiguous.
    pub fn of_name(handle: &str, components: &[String]) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = fxhash::FxHasher64::default();
        (handle, components).hash(&mut hasher);
        Self(hasher.finish())
    }

    /// The key that `text` spells, all of it.
    fn parse(text: &str) -> Option<Self> {
        (text.len() == 16 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| u64::from_str_radix(text, 16).ok().map(Self))
            .flatten()
    }

    /// The key at the start of `text` and what follows its separator.
    fn split(text: &str) -> Option<(Self, &str)> {
        let key = Self::parse(text.get(..16)?)?;
        Some((key, text.get(16..)?.strip_prefix('$')?))
    }
}

impl std::fmt::Display for DefinitionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
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
    Template {
        definition: DefinitionId,
        member: String,
    },
    /// Member `member` of the instance of interface `interface` that an
    /// access selects: analysis builds each access once with it, for the
    /// frontend to repeat for every instance of the elaborated design (see
    /// [`crate::ir::Module::expand_instances`]).
    Placeholder {
        definition: DefinitionId,
        member: String,
    },
    /// The id of that instance, which the access compares its handle with.
    Instance { definition: DefinitionId },
    /// Static variable `name` of a subroutine of the instance of interface
    /// `interface` that a call through a virtual interface selects, which
    /// the frontend repeats for every instance, as a member.
    StaticPlaceholder {
        definition: DefinitionId,
        name: String,
    },
    /// Static variable `name` of a subroutine of interface instance `id`,
    /// called through a virtual interface (13.3.2, 25.9): one per instance,
    /// held by the scope that makes the call.
    Static { id: u32, name: String },
    /// The object that hierarchical name `name` (see
    /// [`DefinitionId::of_name`]) of the body of an instance of interface
    /// `definition` denotes, found from the scope that instantiates it
    /// (23.8): held by that scope, which the frontend binds to the object.
    Upward {
        definition: DefinitionId,
        name: DefinitionId,
    },
}

impl VirtualInterfaceSignal {
    /// The kind of a signal named `name`, if the frontend binds it.
    pub fn of(name: &str) -> Option<Self> {
        if let Some((definition, name)) = name
            .strip_prefix(UPWARD_PREFIX)
            .and_then(DefinitionId::split)
        {
            return Some(Self::Upward {
                definition,
                name: DefinitionId::parse(name)?,
            });
        }
        if let Some((definition, name)) = name
            .strip_prefix(STATIC_PLACEHOLDER_PREFIX)
            .and_then(DefinitionId::split)
        {
            return Some(Self::StaticPlaceholder {
                definition,
                name: name.to_owned(),
            });
        }
        if let Some((id, name)) = name
            .strip_prefix(STATIC_PREFIX)
            .and_then(|rest| rest.split_once('$'))
        {
            return Some(Self::Static {
                id: id.parse().ok()?,
                name: name.to_owned(),
            });
        }
        if let Some((definition, member)) = name
            .strip_prefix(PLACEHOLDER_PREFIX)
            .and_then(DefinitionId::split)
        {
            return Some(Self::Placeholder {
                definition,
                member: member.to_owned(),
            });
        }
        if let Some(definition) = name
            .strip_prefix(INSTANCE_PREFIX)
            .and_then(DefinitionId::parse)
        {
            return Some(Self::Instance { definition });
        }
        if let Some(handle) = name.strip_prefix(IDENTITY_PREFIX) {
            return Some(Self::Identity {
                handle: handle.to_owned(),
            });
        }
        if let Some((definition, member)) = name
            .strip_prefix(TEMPLATE_PREFIX)
            .and_then(DefinitionId::split)
        {
            return Some(Self::Template {
                definition,
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
            Self::Template { definition, member } => {
                format!("{TEMPLATE_PREFIX}{definition}${member}")
            }
            Self::Placeholder { definition, member } => {
                format!("{PLACEHOLDER_PREFIX}{definition}${member}")
            }
            Self::Instance { definition } => format!("{INSTANCE_PREFIX}{definition}"),
            Self::StaticPlaceholder { definition, name } => {
                format!("{STATIC_PLACEHOLDER_PREFIX}{definition}${name}")
            }
            Self::Static { id, name } => format!("{STATIC_PREFIX}{id}${name}"),
            Self::Upward { definition, name } => format!("{UPWARD_PREFIX}{definition}${name}"),
        }
    }
}

/// The member of an interface's declaration that hierarchical name
/// `components` of its tasks and functions denotes, once the hierarchy
/// tells each instance's object (see
/// [`VirtualInterfaceDeclaration::outward`]).
pub fn outward_member(components: &[String]) -> String {
    format!(
        "{OUTWARD_MEMBER_PREFIX}{}",
        DefinitionId::of_name("", components)
    )
}

/// The start of the name of an [`outward_member`].
const OUTWARD_MEMBER_PREFIX: &str = "$up$";

/// The signal that hierarchical name `components` of the body of instance
/// `handle` of interface `interface` denotes in the scope that instantiates
/// the instance, see [`VirtualInterfaceSignal::Upward`].
pub fn upward_signal(interface: &str, handle: &str, components: &[String]) -> String {
    VirtualInterfaceSignal::Upward {
        definition: DefinitionId::of(interface),
        name: DefinitionId::of_name(handle, components),
    }
    .name()
}

/// The type of a virtual interface value: an unsigned two-state number,
/// bound to its semantic type in `table`, which tells its interface and
/// modport (IEEE 1800-2023 25.9) and which an array of it keeps as the type
/// of its elements.
pub(super) fn value_type(table: &mut semantic_types::TyTable, interface: VirtualInterface) -> Type {
    let id = interface.type_id(table);
    let mut ty = Type::new(TypeKind::Bit);
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal((WIDTH - 1).to_string()),
        ConstExpr::Literal("0".into()),
    ));
    ty.bind(table, id);
    ty
}

/// The declarations of the design's virtual interfaces, if it has any.
pub(super) fn declarations(state: &Session) -> Option<&VirtualInterfaces> {
    state.virtual_interfaces.as_deref()
}

/// The type `virtual name[#(arguments)][.modport]`, checked against the
/// interface; `context` evaluates the parameter values, which a type of a
/// parameterized interface holds with the defaults of those it does not
/// assign (IEEE 1800-2023 25.9).
pub(super) fn bind_type(
    tree: &AnalysisContext<'_>,
    native: &celox_sv_syntax::Declarations,
    state: &mut Session,
    name: &celox_sv_syntax::Name,
    arguments: Option<&[celox_sv_syntax::Argument]>,
    modport: Option<&celox_sv_syntax::Name>,
    context: &PackedDimensions,
) -> Converted<Type> {
    let interface = instances::normalize_identifier(&name.spelling);
    let modport = modport.map(|modport| instances::normalize_identifier(&modport.spelling));
    // The packages are analyzed once to find the interfaces before the
    // declarations exist; the analysis that uses the types checks them.
    let Some(declarations) = declarations(state) else {
        return Ok(value_type(
            &mut state.types,
            VirtualInterface::new(interface, modport, Arc::new([])),
        ));
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
    if let Some(modport) = &modport
        && !declaration.modports.contains_key(modport)
    {
        return Err(unsupported(format!(
            "modport `{modport}` of interface `{interface}`"
        )));
    }
    let values = match &declaration.template {
        Some(template) => {
            let template = template.clone();
            let overrides = argument_values(
                tree, native, state, &template, &interface, arguments, context,
            )?;
            let overrides = overrides
                .into_iter()
                .map(|(name, value)| (name, parameters::const_expr_from_i128(value)))
                .collect();
            specialization(state, &interface, &template, &overrides)?
        }
        None if declaration.parameterized => {
            return Err(unsupported(format!(
                "virtual interface type of parameterized interface `{interface}`"
            )));
        }
        None if arguments.is_some_and(|arguments| !arguments.is_empty()) => {
            return Err(unsupported(format!(
                "parameter value assignment in a virtual interface type of interface `{interface}`, which has no parameters"
            )));
        }
        None => Arc::new([]),
    };
    Ok(value_type(
        &mut state.types,
        VirtualInterface::new(interface, modport, values),
    ))
}

/// The values that parameter value assignment `arguments` of a virtual
/// interface type of `interface` gives its parameters, by name.
fn argument_values(
    tree: &AnalysisContext<'_>,
    native: &celox_sv_syntax::Declarations,
    state: &mut Session,
    template: &DeclarationTemplate,
    interface: &str,
    arguments: Option<&[celox_sv_syntax::Argument]>,
    context: &PackedDimensions,
) -> Converted<HashMap<String, i128>> {
    let mut values = HashMap::default();
    let Some(arguments) = arguments else {
        return Ok(values);
    };
    let overridable = template
        .overridable
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let ordered = arguments.iter().all(|argument| argument.name.is_none());
    if !ordered && arguments.iter().any(|argument| argument.name.is_none()) {
        return Err(unsupported("mixed named and ordered parameter assignment"));
    }
    for (position, argument) in arguments.iter().enumerate() {
        let parameter = match &argument.name {
            Some(name) => instances::normalize_identifier(&name.spelling),
            None => overridable
                .get(position)
                .map(|name| (*name).to_owned())
                .ok_or_else(|| AnalyzerError::UndeclaredIdentifier {
                    name: format!("#{}", position + 1),
                    context: format!("interface `{interface}` has fewer parameters"),
                })?,
        };
        if !overridable.contains(&parameter.as_str()) {
            return Err(AnalyzerError::UndeclaredIdentifier {
                name: parameter,
                context: format!("no parameter of interface `{interface}` has this name"),
            });
        }
        // `.name()` keeps the default (23.10.2.2).
        let Some(id) = argument.value else {
            continue;
        };
        let value = constant_binding::bind_in(id, native, tree, state, context)?
            .and_then(|expr| {
                eval_ast_const_expr(&tree.functions, &expr, &context.const_env).or_else(|| {
                    // A constant of another package, which a type declared
                    // in a package names (`c::aw`), is one of the scope's
                    // imported constants, under those of the context.
                    let env = ConstEnv::clone(&context.const_env)
                        .union(tree.imported.const_env.clone());
                    eval_ast_const_expr(&tree.functions, &expr, &env)
                })
            })
            .ok_or_else(|| {
                unsupported(format!(
                    "value of parameter `{parameter}` of a virtual interface type of `{interface}` that is not an integral constant"
                ))
            })?;
        if values.insert(parameter.clone(), value).is_some() {
            return Err(unsupported(format!(
                "duplicate parameter override `{parameter}`"
            )));
        }
    }
    Ok(values)
}

/// The parameter values of the specialization of `interface` whose
/// parameter value assignment gives `overrides`, as the analysis of its
/// signature evaluates them, with its declaration added to the design's if
/// it lacks it, so that the scopes analyzed from now on declare its
/// placeholders and the frontend numbers its instances (25.9).
pub(super) fn specialization(
    state: &mut Session,
    interface: &str,
    template: &DeclarationTemplate,
    overrides: &HashMap<String, ConstExpr>,
) -> Converted<Arc<[i128]>> {
    let mut assignment = overrides.iter().collect::<Vec<_>>();
    assignment.sort_by(|left, right| left.0.cmp(right.0));
    let request = format!("{interface}#{assignment:?}");
    if let Some(values) =
        declarations(state).and_then(|declarations| declarations.requests.get(&request))
    {
        return Ok(values.clone());
    }
    let (values, members) = template.specialize(interface, overrides)?;
    let key = specialization_key(interface, &values);
    if let Some(declarations) = state.virtual_interfaces.as_mut() {
        let declarations = Arc::make_mut(declarations);
        if !declarations.interfaces.contains_key(&key)
            && let Some(base) = declarations.interfaces.get(interface)
        {
            let specialized = base.specialization(&values, members);
            declarations.interfaces.insert(key, specialized);
        }
        declarations.requests.insert(request, values.clone());
    }
    Ok(values)
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
        .and_then(|declarations| declarations.interfaces.get(&ty.key()))
        .ok_or_else(|| unsupported(format!("virtual interface type of `{}`", ty.spelling())))?;
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
        definition: DefinitionId::of(&ty.key()),
    }
    .name();
    let signal = VirtualInterfaceSignal::Placeholder {
        definition: DefinitionId::of(&ty.key()),
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
        .and_then(|declarations| declarations.interfaces.get(&ty.key()))
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
/// signals of each instance (see [`crate::ir::Module::expand_instances`]).
pub(super) fn member_signals(
    declarations: &VirtualInterfaces,
    table: &mut semantic_types::TyTable,
) -> Vec<Signal> {
    let mut interfaces = declarations.interfaces.iter().collect::<Vec<_>>();
    interfaces.sort_by(|left, right| left.0.cmp(right.0));
    let mut signals = Vec::new();
    for (interface, declaration) in &interfaces {
        signals.extend(declaration.members.iter().map(|member| Signal {
            name: template(interface, &member.name),
            r#type: member.type_in(table),
            is_net: false,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            is_const: false,
            package_variable: None,
            hierarchical: None,
            force_control: None,
        }));
    }
    for (interface, declaration) in interfaces {
        let instance = VirtualInterfaceSignal::Instance {
            definition: DefinitionId::of(interface),
        }
        .name();
        signals.push(Signal {
            hierarchical: Some(instance.clone()),
            force_control: None,
            name: instance,
            r#type: value_type(
                table,
                VirtualInterface::new(
                    declaration.interface.clone(),
                    None,
                    declaration.arguments.clone(),
                ),
            ),
            is_net: false,
            unresolved_net: false,
            net_resolution: NetResolution::Wire,
            net_drive: NetDrive::None,
            is_const: false,
            package_variable: None,
        });
        signals.extend(declaration.members.iter().map(|member| {
            let name = VirtualInterfaceSignal::Placeholder {
                definition: DefinitionId::of(interface),
                member: member.name.clone(),
            }
            .name();
            // It denotes the member of another instance, as a hierarchical
            // name does: that instance's object, not one of this module.
            Signal {
                hierarchical: Some(name.clone()),
                force_control: None,
                name,
                r#type: member.type_in(table),
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

/// The placeholder signals of the specializations of parameterized
/// interfaces registered so far that a scope with signals `signals` lacks,
/// if it reaches the instances through virtual interfaces at all: the
/// packages declare those of the specializations they name, and each
/// analysis those that it or an earlier one names (25.9).
pub(super) fn specialization_signals<'a>(
    state: &mut Session,
    signals: impl Iterator<Item = &'a Signal>,
) -> Vec<Signal> {
    let Some(declarations) = state.virtual_interfaces.clone() else {
        return Vec::new();
    };
    if declarations
        .interfaces
        .values()
        .all(|declaration| declaration.arguments.is_empty() && !declaration.has_outward_members())
    {
        return Vec::new();
    }
    let mut reaches = false;
    let known = signals
        .map(|signal| {
            reaches |= is_placeholder_signal(&signal.name);
            signal.name.as_str()
        })
        .collect::<HashSet<_>>();
    if !reaches {
        return Vec::new();
    }
    let specialized = VirtualInterfaces {
        interfaces: declarations
            .interfaces
            .iter()
            .filter(|(_, declaration)| {
                !declaration.arguments.is_empty() || declaration.has_outward_members()
            })
            .map(|(key, declaration)| (key.clone(), declaration.clone()))
            .collect(),
        requests: HashMap::default(),
    };
    member_signals(&specialized, &mut state.types)
        .into_iter()
        .filter(|signal| !known.contains(signal.name.as_str()))
        .collect()
}

/// The signal a select through member `member` of a virtual interface of
/// interface `interface` is bound against (see [`selected_read`]).
pub(super) fn template(interface: &str, member: &str) -> String {
    VirtualInterfaceSignal::Template {
        definition: DefinitionId::of(interface),
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
    let from = template(&ty.key(), member);
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

/// `expr` with each occurrence of expression `from` replaced by `to`.
pub(super) fn replace(expr: Expr, from: &Expr, to: &Expr) -> Expr {
    if expr == *from {
        return to.clone();
    }
    map_children(expr, &mut |child| replace(child, from, to))
}

/// `expr` with identifier `from` renamed to `to`.
pub(super) fn rename(expr: Expr, from: &str, to: &str) -> Expr {
    match expr {
        Expr::Ident(name) if name == from => Expr::Ident(to.to_owned()),
        expr => map_children(expr, &mut |child| rename(child, from, to)),
    }
}

/// The name of the subroutine that runs task or function `subroutine` of
/// interface `interface` for the instance a virtual interface denotes,
/// which it takes as its first argument.
pub(crate) fn subroutine_name(interface: &str, subroutine: &str) -> String {
    format!("{interface}$vif${subroutine}")
}

/// The subroutine that calls task or function `name` of the interface that
/// virtual interface `handle` denotes (IEEE 1800-2023 25.9), with the names
/// of its formals after the handle; `None` if `handle` is no virtual
/// interface or its interface declares no such subroutine. Through a
/// modport, only a subroutine the modport imports may be called (25.7).
pub(super) fn method(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    handle: celox_sv_syntax::ExprId,
    name: &str,
    native: &celox_sv_syntax::Declarations,
    dimensions: &PackedDimensions,
) -> Converted<Option<(String, Vec<Option<String>>)>> {
    let Some(ty) = native_type(tree, session, handle, native, dimensions) else {
        return Ok(None);
    };
    let Some(declaration) =
        declarations(session).and_then(|declarations| declarations.interfaces.get(&ty.key()))
    else {
        return Ok(None);
    };
    if declaration.dpi_imports.contains(name) {
        return Err(unsupported(format!(
            "DPI import `{name}` of interface `{}` called through a virtual interface",
            ty.interface
        )));
    }
    let Some(formals) = declaration.subroutines.get(name) else {
        return Ok(None);
    };
    // The interface's subroutines are built once each instance's objects of
    // their names are known (see `VirtualInterfaceDeclaration::outward`).
    if !declaration.outward.is_empty() {
        return Err(AnalyzerError::VirtualInterfaceOutward {
            interface: ty.key(),
        });
    }
    if let Some(modport) = &ty.modport
        && !declaration
            .modport_imports
            .get(modport)
            .is_some_and(|imports| imports.iter().any(|import| import == name))
    {
        return Err(unsupported(format!(
            "call of `{name}` through virtual interface type `{}`, whose modport does not import it",
            ty.spelling()
        )));
    }
    let formals = formals.iter().cloned().map(Some).collect();
    Ok(Some((subroutine_name(&ty.key(), name), formals)))
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
    let decl = types.decl(*interface);
    let arguments = decl
        .arguments
        .iter()
        .flat_map(|arguments| arguments.iter())
        .map(|argument| match argument {
            semantic_types::ClassArgument::Value(Some(value)) => Some(*value),
            _ => None,
        })
        .collect::<Option<Arc<[i128]>>>()?;
    Some(Arc::new(VirtualInterface::new(
        decl.name.to_string(),
        modport.as_deref().map(str::to_owned),
        arguments,
    )))
}

/// Whether declared shape `shape` is a virtual interface or a fixed-size
/// array of them (25.9), as its type tells.
pub(super) fn holds_virtual_interface(
    shape: &VariableDimensions,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    dims: &PackedDimensions,
) -> bool {
    shape_holds_virtual_interface(shape, &mut session.types, &tree.functions, &dims.const_env)
}

/// [`holds_virtual_interface`] with the constants of `env`. A shape whose
/// type cannot be formed in `env` is told by the type of its elements, which
/// the shape keeps.
fn shape_holds_virtual_interface(
    shape: &VariableDimensions,
    table: &mut semantic_types::TyTable,
    functions: &const_functions::ConstantFunctions,
    env: &ConstEnv,
) -> bool {
    match shape.type_id(table, functions, env) {
        Some(ty) => of_type(table, ty).is_some() || element_of_array(table, ty).is_some(),
        None => {
            shape.container().is_none()
                && shape
                    .element_type(table)
                    .is_some_and(|element| of_type(table, element).is_some())
        }
    }
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
    let found = found.clone().type_in(&mut session.types);
    check_pure(&handle)?;
    let mut arms = Vec::new();
    for (id, signal) in signals {
        arms.push((id, select(Expr::Ident(signal))?));
    }
    // The width of the selected member as the scope reads it, or else as
    // the interface declares it: a scope outside the instances, such as a
    // package's class, does not see their signals.
    let declared = || {
        casts::expr_type_from_type(functions, &found, &ConstEnv::default()).map(|shape| shape.width)
    };
    let (width, signed) = match arms.first() {
        Some((_, value)) => (
            expr_static_width(tree, value, dimensions).or_else(declared),
            dimensions::expression_signedness(value, dimensions, &dimensions.const_env)
                .unwrap_or_else(|| found.is_signed()),
        ),
        None => (declared(), found.is_signed()),
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
pub(super) fn identity_signal(
    state: &mut Session,
    handle: &str,
    interface: &str,
    arguments: &Arc<[i128]>,
) -> Option<Signal> {
    declarations(state)?.interfaces.get(interface)?;
    Some(Signal {
        name: VirtualInterfaceSignal::Identity {
            handle: handle.to_owned(),
        }
        .name(),
        r#type: value_type(
            &mut state.types,
            VirtualInterface::new(interface.to_owned(), None, arguments.clone()),
        ),
        is_net: false,
        unresolved_net: false,
        net_resolution: NetResolution::Wire,
        net_drive: NetDrive::None,
        is_const: false,
        package_variable: None,
        hierarchical: None,
        force_control: None,
    })
}

/// [`identity_signal`] for a port of interface `interface`, unless the
/// interface declares no members: the frontend finds the instance a port
/// denotes by the connections of its members.
pub(super) fn port_identity_signal(
    state: &mut Session,
    handle: &str,
    interface: &str,
    arguments: &Arc<[i128]>,
) -> Option<Signal> {
    if declarations(state)?
        .interfaces
        .get(interface)?
        .members
        .is_empty()
    {
        return None;
    }
    identity_signal(state, handle, interface, arguments)
}

/// The values of the parameters of an instance or port of interface
/// `interface` whose parameter value assignment gives `values`, by which a
/// virtual interface of the specialization they select may denote it
/// (25.9), with the specialization registered; empty for an interface
/// without parameters or that no virtual interface type names.
pub(super) fn instance_arguments(
    state: &mut Session,
    interface: &str,
    values: &HashMap<String, ConstExpr>,
) -> Converted<Arc<[i128]>> {
    let Some(template) = declarations(state)
        .and_then(|declarations| declarations.interfaces.get(interface))
        .and_then(|declaration| declaration.template.clone())
    else {
        return Ok(Arc::new([]));
    };
    specialization(state, interface, &template, values)
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
    let Some(interface) = scope::imported(tree).interface_types.get(name).cloned() else {
        return Ok(None);
    };
    let members = declarations(session)
        .and_then(|declarations| declarations.interfaces.get(&interface))
        .is_some_and(|declaration| !declaration.members.is_empty());
    if !members {
        // A port of an interface without members has no connection the
        // instance it denotes could be found by (see
        // [`port_identity_signal`]).
        return Err(unsupported(format!(
            "virtual interface value of port `{name}` of interface `{interface}`, which declares no members"
        )));
    }
    // Every other port and instance has an identity signal under its own
    // name, but the one of an instance in a generate block is named by its
    // hierarchical name (#4266).
    Err(unsupported(format!(
        "virtual interface value of `{name}`, an instance of interface `{interface}` named by its simple name inside the generate block that declares it"
    )))
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
            // An instance of a specialization is one of the instances that
            // virtual interfaces of that specialization select among.
            let interface = &specialization_key(
                interface,
                symbols
                    .interface_arguments
                    .get(handle)
                    .map_or(&[][..], |arguments| arguments),
            );
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
                blocks: symbols
                    .interface_blocks
                    .get(handle)
                    .map_or_else(Vec::new, |blocks| blocks.to_vec()),
            })
        })
        .collect::<Vec<_>>();
    handles.sort_by(|left, right| left.handle.cmp(&right.handle));
    handles
}

/// Every interface instance and port of a module with `symbols` and `ports`,
/// each with the signals or ports that hold its members (see
/// [`crate::ir::Module::home_handles`]).
pub(super) fn home_handles(
    symbols: &scope::ScopeSymbols,
    ports: &[Port],
) -> Vec<crate::ir::InterfaceHandle> {
    let mut handles = symbols
        .interface_types
        .iter()
        .map(|(handle, interface)| {
            // The members of the handle are the paths it starts, as
            // `handles` spells them (`g[0].bus.x` for `g[0].bus`).
            let prefix = format!("{handle}.");
            let mut members: Vec<(String, String)> = symbols
                .interface_members
                .iter()
                .filter_map(|(path, signal)| {
                    path.strip_prefix(&prefix)
                        .map(|member| (member.to_owned(), signal.clone()))
                })
                .collect();
            members.sort();
            let port = members
                .iter()
                .any(|(_, signal)| ports.iter().any(|port| port.name() == signal));
            crate::ir::InterfaceHandle {
                handle: handle.clone(),
                interface: interface.clone(),
                port,
                members,
                blocks: Vec::new(),
            }
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
                && let Some(container) =
                    containers::value_container(*receiver, native, tree, session, dimensions)
                && let semantic_types::TyData::Array { element, .. } = session.types.data(container)
                && let Some(element) = of_type(&session.types, *element)
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
            return Ok(Some(VirtualInterface::new(
                interface.clone(),
                imported.interface_modports.get(handle).cloned(),
                imported
                    .interface_arguments
                    .get(handle)
                    .cloned()
                    .unwrap_or_else(|| Arc::new([])),
            )));
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
        Some(source) if source.key() == interface => Ok(()),
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
        let Err(actual) = equality_operand(tree, session, other, native, dimensions, &ty.key())
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
    let found = found.clone().type_in(&mut session.types);
    let width = casts::expr_type_from_type(functions, &found, &ConstEnv::default())
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

/// Whether member `member` that a virtual interface of type `ty` reaches is
/// a named event (15.5).
pub(super) fn is_event_member(
    state: &Session,
    ty: &VirtualInterface,
    member: &str,
) -> Converted<bool> {
    let (found, _) = self::member(state, ty, member)?;
    Ok(matches!(&found.r#type, MemberType::Handle(class) if class == events::CLASS))
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
pub(in crate::ast) fn is_event_block(stmt: &Stmt) -> bool {
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
pub(crate) fn placeholder_interface(name: &str) -> Option<DefinitionId> {
    match VirtualInterfaceSignal::of(name)? {
        VirtualInterfaceSignal::Placeholder { definition, .. }
        | VirtualInterfaceSignal::StaticPlaceholder { definition, .. }
        | VirtualInterfaceSignal::Instance { definition } => Some(definition),
        _ => None,
    }
}

/// Whether `name` is a placeholder signal (see [`member_signals`]).
pub(crate) fn is_placeholder_signal(name: &str) -> bool {
    placeholder_interface(name).is_some()
}

#[cfg(test)]
mod holds_virtual_interface_tests {
    use super::*;

    /// `[N-1:0]`, a range that names `N`.
    fn symbolic_range() -> UnpackedRange {
        UnpackedRange::new(
            ConstExpr::Binary {
                left: Box::new(ConstExpr::Ident("N".into())),
                op: BinaryOp::Sub,
                right: Box::new(ConstExpr::Literal("1".into())),
            },
            ConstExpr::Literal("0".into()),
        )
    }

    #[test]
    fn shapes_whose_type_is_not_formed_keep_their_virtual_interface_element_type() {
        // `virtual I handles[N]` where the scope has no `N`.
        let mut table = semantic_types::TyTable::default();
        let mut ty = value_type(
            &mut table,
            VirtualInterface::new("I".into(), None, Arc::new([])),
        );
        ty.unpacked_ranges_mut().push(symbolic_range());
        let shape = procedural::dimensions_from_type(&ty);
        let functions = const_functions::ConstantFunctions::default();
        assert!(
            shape
                .type_id(&mut table, &functions, &ConstEnv::default())
                .is_none()
        );
        assert!(shape_holds_virtual_interface(
            &shape,
            &mut table,
            &functions,
            &ConstEnv::default()
        ));
        let env = ConstEnv::from_iter([("N".into(), 2)]);
        assert!(shape_holds_virtual_interface(
            &shape, &mut table, &functions, &env
        ));
    }

    /// An array keeps the semantic type of its virtual interface elements
    /// (IEEE 1800-2023 25.9, 7.4) when its unpacked dimensions change, and
    /// its shapes and selected elements keep it too; another change of the
    /// type forgets it.
    #[test]
    fn arrays_keep_the_semantic_type_of_their_virtual_interface_elements() {
        let mut table = semantic_types::TyTable::default();
        let functions = const_functions::ConstantFunctions::default();
        let env = ConstEnv::default();
        let interface = VirtualInterface::new("bus_if".into(), Some("mp".into()), Arc::new([]));
        let element = interface.type_id(&mut table);
        let mut ty = value_type(&mut table, interface);
        let literal = |value: &str| ConstExpr::Literal(value.into());
        ty.unpacked_ranges_mut()
            .push(UnpackedRange::new(literal("0"), literal("3")));
        assert!(ty.binding.get(&table).is_none());
        assert_eq!(ty.element.get(&table), Some(element));
        let id = semantic_types::of_type(&mut table, &functions, &ty, &env).unwrap();
        assert_eq!(table.unpacked_element(id, 1), Some(element));
        // The shape of a variable of the array, and an element selected
        // from it before the shape is bound.
        let shape = procedural::dimensions_from_type(&ty);
        assert_eq!(shape.element_type(&table), Some(element));
        assert_eq!(
            shape.unpacked_element(&table).element_type(&table),
            Some(element)
        );
        // Any other change of the type may change its elements.
        ty.is_signed = true;
        assert!(ty.element.get(&table).is_none());
    }

    /// A virtual interface type has the shape of the number that holds its
    /// value; its element type tells it apart from that number and from
    /// other interfaces and modports (IEEE 1800-2023 25.9).
    #[test]
    fn virtual_interface_types_are_equal_by_their_interface_and_modport() {
        let mut table = semantic_types::TyTable::default();
        let mut vif = |interface: &str, modport: Option<&str>| {
            value_type(
                &mut table,
                VirtualInterface::new(interface.into(), modport.map(Into::into), Arc::new([])),
            )
        };
        let (bus, again, mp, other) = (
            vif("bus_if", None),
            vif("bus_if", None),
            vif("bus_if", Some("mp")),
            vif("other_if", None),
        );
        let mut number = Type::new(TypeKind::Bit);
        number.packed_ranges.push(PackedRange::new(
            ConstExpr::Literal((WIDTH - 1).to_string()),
            ConstExpr::Literal("0".into()),
        ));
        assert_eq!(bus, again);
        assert_ne!(bus, mp);
        assert_ne!(bus, other);
        assert_ne!(bus, number);
        let shape = |ty: &Type| procedural::dimensions_from_type(ty);
        assert_eq!(shape(&bus), shape(&again));
        assert_ne!(shape(&bus), shape(&number));
    }
}
